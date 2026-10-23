/**
 * Audio graph and procedural cue bank (task T-049, plan §8.8).
 *
 * Graph: buses → compressor (-12 dB, 4:1) → master → destination. The sfx bus
 * feeds at most 24 panners; the ui bus is stereo. There is no music in v1.
 *
 * The plan budgets 30 OGG files. This build has no licensed/recorded assets, so
 * the same 30 cue names are synthesised from oscillators and a shared noise
 * buffer instead. Payload cost is zero bytes, and the API is the same one a
 * sample-backed bank would use.
 */

import { RUMBLE_PROCESSOR, RUMBLE_WORKLET_URL } from "./worklets/rumble";

export type AudioTier = "low" | "med" | "high";

export interface AudioPosition {
  x: number;
  y: number;
  z: number;
}

export interface AudioStats {
  available: boolean;
  state: AudioContextState | "unavailable";
  voices: number;
  maxVoices: number;
  cues: number;
  buses: string[];
  compressorThreshold: number;
  compressorRatio: number;
  pannerModel: "equalpower" | "HRTF" | "none";
  workletReady: boolean;
  rumble: number;
  lastCue: string | null;
  lastError: string;
}

interface CueSpec {
  frequency: number;
  endFrequency: number;
  duration: number;
  wave: OscillatorType;
  volume: number;
  noise: boolean;
  bus: "sfx" | "ui";
}

const CUES: Record<string, CueSpec> = {
  block_break: { frequency: 180, endFrequency: 70, duration: 0.12, wave: "square", volume: 0.28, noise: true, bus: "sfx" },
  block_place: { frequency: 260, endFrequency: 180, duration: 0.07, wave: "triangle", volume: 0.24, noise: false, bus: "sfx" },
  footstep_1: { frequency: 90, endFrequency: 60, duration: 0.06, wave: "sine", volume: 0.12, noise: true, bus: "sfx" },
  footstep_2: { frequency: 100, endFrequency: 65, duration: 0.06, wave: "sine", volume: 0.12, noise: true, bus: "sfx" },
  footstep_3: { frequency: 84, endFrequency: 55, duration: 0.06, wave: "sine", volume: 0.12, noise: true, bus: "sfx" },
  footstep_4: { frequency: 108, endFrequency: 70, duration: 0.06, wave: "sine", volume: 0.12, noise: true, bus: "sfx" },
  jump: { frequency: 300, endFrequency: 520, duration: 0.12, wave: "triangle", volume: 0.18, noise: false, bus: "sfx" },
  land: { frequency: 120, endFrequency: 60, duration: 0.1, wave: "sine", volume: 0.2, noise: true, bus: "sfx" },
  sword_swing: { frequency: 700, endFrequency: 240, duration: 0.11, wave: "sawtooth", volume: 0.16, noise: true, bus: "sfx" },
  sword_flesh: { frequency: 220, endFrequency: 90, duration: 0.1, wave: "square", volume: 0.3, noise: true, bus: "sfx" },
  sword_crate: { frequency: 380, endFrequency: 140, duration: 0.09, wave: "square", volume: 0.26, noise: true, bus: "sfx" },
  bow_draw: { frequency: 160, endFrequency: 320, duration: 0.25, wave: "triangle", volume: 0.12, noise: true, bus: "sfx" },
  bow_release: { frequency: 500, endFrequency: 180, duration: 0.12, wave: "sawtooth", volume: 0.18, noise: false, bus: "sfx" },
  arrow_hit: { frequency: 240, endFrequency: 90, duration: 0.08, wave: "square", volume: 0.24, noise: true, bus: "sfx" },
  staff_cast: { frequency: 520, endFrequency: 180, duration: 0.32, wave: "sine", volume: 0.2, noise: false, bus: "sfx" },
  heal: { frequency: 440, endFrequency: 880, duration: 0.4, wave: "sine", volume: 0.16, noise: false, bus: "sfx" },
  buddy_spawn: { frequency: 330, endFrequency: 660, duration: 0.35, wave: "triangle", volume: 0.2, noise: false, bus: "sfx" },
  buddy_death: { frequency: 220, endFrequency: 55, duration: 0.5, wave: "sawtooth", volume: 0.22, noise: true, bus: "sfx" },
  tower_fire: { frequency: 180, endFrequency: 120, duration: 0.16, wave: "square", volume: 0.18, noise: true, bus: "sfx" },
  ram_impact: { frequency: 70, endFrequency: 45, duration: 0.3, wave: "sine", volume: 0.32, noise: true, bus: "sfx" },
  catapult_launch: { frequency: 140, endFrequency: 70, duration: 0.28, wave: "triangle", volume: 0.24, noise: true, bus: "sfx" },
  catapult_impact: { frequency: 90, endFrequency: 40, duration: 0.35, wave: "sine", volume: 0.34, noise: true, bus: "sfx" },
  core_hit: { frequency: 80, endFrequency: 40, duration: 0.45, wave: "sine", volume: 0.38, noise: true, bus: "sfx" },
  core_destroyed: { frequency: 60, endFrequency: 28, duration: 1.2, wave: "sawtooth", volume: 0.42, noise: true, bus: "sfx" },
  recipe_complete: { frequency: 523, endFrequency: 1046, duration: 0.45, wave: "sine", volume: 0.22, noise: false, bus: "sfx" },
  tier_unlock: { frequency: 392, endFrequency: 784, duration: 0.6, wave: "triangle", volume: 0.22, noise: false, bus: "ui" },
  victory: { frequency: 523, endFrequency: 1046, duration: 1.4, wave: "triangle", volume: 0.3, noise: false, bus: "ui" },
  defeat: { frequency: 330, endFrequency: 110, duration: 1.4, wave: "sawtooth", volume: 0.26, noise: false, bus: "ui" },
  ui_click: { frequency: 660, endFrequency: 660, duration: 0.035, wave: "square", volume: 0.12, noise: false, bus: "ui" },
  ui_error: { frequency: 160, endFrequency: 120, duration: 0.18, wave: "square", volume: 0.16, noise: false, bus: "ui" },
};

const MAX_VOICES = 24;
const MAX_DISTANCE = 40;

interface Voice {
  source: AudioScheduledSourceNode;
  gain: GainNode;
  panner: PannerNode | null;
  cue: string;
}

export class AudioEngine {
  readonly cueCount = Object.keys(CUES).length;
  readonly maxVoices = MAX_VOICES;

  private readonly context: AudioContext | null;
  private readonly master: GainNode | null;
  private readonly compressor: DynamicsCompressorNode | null;
  private readonly sfxBus: GainNode | null;
  private readonly uiBus: GainNode | null;
  private readonly musicBus: GainNode | null;
  private readonly noise: AudioBuffer | null;
  private readonly voices: Voice[] = [];
  private readonly pannerModel: "equalpower" | "HRTF" | "none";
  private worklet: AudioWorkletNode | null = null;
  private workletReady = false;
  private rumbleLevel = 0;
  private lastError = "";
  private lastCue: string | null = null;
  private playCount = 0;

  constructor(tier: AudioTier, factory?: () => AudioContext) {
    const create =
      factory ??
      (() => {
        const Ctor =
          window.AudioContext ??
          (window as unknown as { webkitAudioContext?: typeof AudioContext }).webkitAudioContext;
        if (Ctor === undefined) throw new Error("no AudioContext");
        return new Ctor();
      });
    try {
      this.context = create();
    } catch (error) {
      this.context = null;
      this.lastError = error instanceof Error ? error.message : String(error);
    }
    this.pannerModel = tier === "low" ? "equalpower" : "HRTF";
    if (this.context === null) {
      this.master = null;
      this.compressor = null;
      this.sfxBus = null;
      this.uiBus = null;
      this.musicBus = null;
      this.noise = null;
      return;
    }
    this.master = this.context.createGain();
    this.master.gain.value = 0.8;
    this.compressor = this.context.createDynamicsCompressor();
    this.compressor.threshold.value = -12;
    this.compressor.ratio.value = 4;
    this.compressor.attack.value = 0.003;
    this.compressor.release.value = 0.25;
    this.sfxBus = this.context.createGain();
    this.uiBus = this.context.createGain();
    this.musicBus = this.context.createGain();
    this.musicBus.gain.value = 0;
    this.sfxBus.connect(this.compressor);
    this.uiBus.connect(this.compressor);
    this.musicBus.connect(this.compressor);
    this.compressor.connect(this.master);
    this.master.connect(this.context.destination);
    this.noise = this.context.createBuffer(1, this.context.sampleRate * 0.5, this.context.sampleRate);
    const samples = this.noise.getChannelData(0);
    for (let i = 0; i < samples.length; i += 1) samples[i] = Math.random() * 2 - 1;
  }

  /** Never call resume outside a user gesture; the caller owns the gate. */
  async unlock(): Promise<boolean> {
    if (this.context === null) return false;
    if (this.context.state === "suspended") {
      try {
        await this.context.resume();
      } catch (error) {
        this.lastError = error instanceof Error ? error.message : String(error);
      }
    }
    return this.context.state === "running";
  }

  play(cue: string, position?: AudioPosition, volume = 1): number {
    if (this.context === null || this.sfxBus === null || this.uiBus === null) return 0;
    const spec = CUES[cue];
    if (spec === undefined) return 0;
    this.lastCue = cue;
    if (position !== undefined) {
      const distance = Math.hypot(position.x, position.y, position.z);
      if (distance > MAX_DISTANCE) return 0;
    }
    if (this.voices.length >= MAX_VOICES) {
      const oldest = this.voices.shift();
      oldest?.source.stop();
    }
    const now = this.context.currentTime;
    const gain = this.context.createGain();
    const source = spec.noise && this.noise !== null
      ? this.context.createBufferSource()
      : this.context.createOscillator();
    if (source instanceof AudioBufferSourceNode && this.noise !== null) {
      source.buffer = this.noise;
    } else if (source instanceof OscillatorNode) {
      source.type = spec.wave;
      source.frequency.setValueAtTime(spec.frequency, now);
      source.frequency.exponentialRampToValueAtTime(
        Math.max(20, spec.endFrequency),
        now + spec.duration,
      );
      source.detune.value = (this.playCount % 7) * 7 - 21;
    }
    const level = Math.max(0.0001, spec.volume * volume);
    gain.gain.setValueAtTime(0.0001, now);
    gain.gain.exponentialRampToValueAtTime(level, now + 0.006);
    gain.gain.exponentialRampToValueAtTime(0.0001, now + spec.duration);
    let panner: PannerNode | null = null;
    if (spec.bus === "sfx" && position !== undefined) {
      panner = this.context.createPanner();
      panner.panningModel = this.pannerModel === "none" ? "equalpower" : this.pannerModel;
      panner.distanceModel = "inverse";
      panner.refDistance = 4;
      panner.maxDistance = MAX_DISTANCE;
      panner.rolloffFactor = 1.2;
      panner.positionX.value = position.x;
      panner.positionY.value = position.y;
      panner.positionZ.value = position.z;
      source.connect(gain).connect(panner).connect(this.sfxBus);
    } else {
      source.connect(gain).connect(spec.bus === "ui" ? this.uiBus : this.sfxBus);
    }
    const voice: Voice = { source, gain, panner, cue };
    this.voices.push(voice);
    source.onended = () => {
      const index = this.voices.indexOf(voice);
      if (index >= 0) this.voices.splice(index, 1);
      panner?.disconnect();
      gain.disconnect();
    };
    source.start(now);
    source.stop(now + spec.duration + 0.02);
    this.playCount += 1;
    return this.playCount;
  }

  /** Map the same 16-byte event slots the juice layer reads. */
  consumeEvents(events: DataView, count: number, listener?: AudioPosition): number {
    const limit = Math.min(count, Math.floor(events.byteLength / 16), 64);
    for (let i = 0; i < limit; i += 1) {
      const at = i * 16;
      this.handleEvent(
        events.getUint32(at, true),
        events.getInt32(at + 4, true),
        events.getInt32(at + 8, true),
        listener,
      );
    }
    return limit;
  }

  /** Set the worklet's intensity for the next quantum. */
  rumble(level: number): void {
    this.rumbleLevel = Math.min(1, Math.max(0, level));
    if (this.context === null) return;
    if (this.worklet === null) {
      if (!this.workletReady && typeof this.context.audioWorklet !== "undefined") {
        this.workletReady = true;
        this.context.audioWorklet
          .addModule(RUMBLE_WORKLET_URL)
          .then(() => {
            if (this.context === null || this.sfxBus === null) return;
            const node = new AudioWorkletNode(this.context, RUMBLE_PROCESSOR, {
              numberOfOutputs: 1,
            });
            node.connect(this.sfxBus);
            this.worklet = node;
          })
          .catch((error: unknown) => {
            this.workletReady = false;
            this.lastError = error instanceof Error ? error.message : String(error);
          });
      }
      return;
    }
    const parameter = this.worklet.parameters.get("intensity");
    parameter?.setTargetAtTime(this.rumbleLevel, this.context.currentTime, 0.02);
  }

  update(dtSeconds: number): void {
    if (this.rumbleLevel <= 0) return;
    this.rumbleLevel = Math.max(0, this.rumbleLevel - dtSeconds * 0.8);
    this.rumble(this.rumbleLevel);
  }

  handleEvent(kind: number, a: number, b: number, listener?: AudioPosition): void {
    switch (kind) {
      case 0:
        this.play("recipe_complete");
        break;
      case 1:
        this.play("buddy_death");
        break;
      case 2:
        this.play("core_hit");
        this.rumble(0.6);
        break;
      case 3:
        this.play("core_destroyed");
        break;
      case 4:
        this.play("land", {
          x: a - (listener?.x ?? 0),
          y: 40 - (listener?.y ?? 0),
          z: b - (listener?.z ?? 0),
        });
        break;
      case 5:
        this.play("sword_flesh");
        break;
      case 6:
        this.play("ui_click");
        break;
      case 7:
        this.play("block_place");
        break;
      case 8:
        // The telegraph must be audible in every camera direction.
        this.play("catapult_launch");
        break;
      case 9:
        this.play("ui_error");
        break;
      case 10:
        this.play("ui_error");
        break;
      default:
        break;
    }
  }

  stats(): AudioStats {
    return {
      available: this.context !== null,
      state: this.context?.state ?? "unavailable",
      voices: this.voices.length,
      maxVoices: MAX_VOICES,
      cues: this.cueCount,
      buses: ["sfx", "ui", "music"],
      compressorThreshold: this.compressor?.threshold.value ?? 0,
      compressorRatio: this.compressor?.ratio.value ?? 0,
      pannerModel: this.pannerModel,
      workletReady: this.worklet !== null,
      rumble: this.rumbleLevel,
      lastCue: this.lastCue,
      lastError: this.lastError,
    };
  }

  /** Test hook: panner position of the most recent voice, or null. */
  lastPanner(): AudioPosition | null {
    const panner = this.voices.at(-1)?.panner;
    if (panner === null || panner === undefined) return null;
    return {
      x: panner.positionX.value,
      y: panner.positionY.value,
      z: panner.positionZ.value,
    };
  }

  dispose(): void {
    for (const voice of this.voices.splice(0)) voice.source.stop();
    void this.context?.close();
  }
}
