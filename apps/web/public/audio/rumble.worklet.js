/**
 * 55 Hz sub-bass rumble (task T-049, plan §8.8).
 *
 * A worklet, not an oscillator graph, because the intensity parameter has to
 * change inside one audio quantum without a gain node click. The only signal is
 * a sine plus a slow LFO; there is no sample loop to decode.
 */
class CbRumble extends AudioWorkletProcessor {
  static get parameterDescriptors() {
    return [
      {
        name: "intensity",
        defaultValue: 0,
        minValue: 0,
        maxValue: 1,
        automationRate: "a-rate",
      },
    ];
  }

  constructor() {
    super();
    this.phase = 0;
    this.lfo = 0;
  }

  process(_inputs, outputs, parameters) {
    const out = outputs[0];
    if (!out || out.length === 0) return true;
    const data = out[0];
    const intensity = parameters.intensity;
    const step = (Math.PI * 2 * 55) / sampleRate;
    const lfoStep = (Math.PI * 2 * 0.7) / sampleRate;
    for (let i = 0; i < data.length; i += 1) {
      const gain = (intensity.length === 1 ? intensity[0] : intensity[i]) ?? 0;
      this.phase += step;
      if (this.phase > Math.PI * 2) this.phase -= Math.PI * 2;
      this.lfo += lfoStep;
      if (this.lfo > Math.PI * 2) this.lfo -= Math.PI * 2;
      const envelope = 0.75 + 0.25 * Math.sin(this.lfo);
      data[i] = Math.sin(this.phase) * gain * envelope * 0.12;
    }
    return true;
  }
}

registerProcessor("cb-rumble", CbRumble);
