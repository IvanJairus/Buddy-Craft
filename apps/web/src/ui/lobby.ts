/**
 * Online lobby shell (task T-039).
 *
 * Networking arrives in T-040; this screen owns the part that must not depend
 * on it: a six-character room code, a copy button, and join validation before
 * any connection is attempted. The start button enters the local match until
 * the net client can replace it with a real room.
 */

function make<K extends keyof HTMLElementTagNameMap>(
  tag: K,
  className?: string,
): HTMLElementTagNameMap[K] {
  const node = document.createElement(tag);
  if (className !== undefined) {
    node.className = className;
  }
  return node;
}

const CODE_ALPHABET = "ABCDEFGHJKLMNPQRSTUVWXYZ23456789";

function generateCode(): string {
  const values = new Uint8Array(6);
  if (typeof crypto !== "undefined" && typeof crypto.getRandomValues === "function") {
    crypto.getRandomValues(values);
  } else {
    for (let i = 0; i < values.length; i += 1) {
      values[i] = Math.floor(Math.random() * 256);
    }
  }
  return Array.from(values, (value) => CODE_ALPHABET[value % CODE_ALPHABET.length]).join("");
}

export function validRoomCode(value: string): boolean {
  return /^[A-Z0-9]{6}$/.test(value.trim().toUpperCase());
}

export interface LobbyCallbacks {
  onBack: () => void;
  onStart: (code: string) => void;
}

export class LobbyScreen {
  readonly element: HTMLDivElement;

  private readonly callbacks: LobbyCallbacks;
  private readonly codeText: HTMLDivElement;
  private readonly copyButton: HTMLButtonElement;
  private readonly joinInput: HTMLInputElement;
  private readonly errorText: HTMLDivElement;
  private readonly startButton: HTMLButtonElement;
  private code = "";

  constructor(callbacks: LobbyCallbacks) {
    this.callbacks = callbacks;
    this.element = make("div", "screen-panel lobby-panel");
    this.element.dataset.screen = "lobby";
    this.element.setAttribute("aria-label", "Online lobby");

    const title = make("h2", "screen-title");
    title.textContent = "Online lobby";

    const create = make("button", "screen-button");
    create.type = "button";
    create.dataset.screenAction = "create-room";
    create.textContent = "CREATE ROOM";
    create.addEventListener("click", () => this.createRoom());

    const codeRow = make("div", "lobby-code-row");
    const codeLabel = make("span", "settings-label");
    codeLabel.textContent = "Room code";
    this.codeText = make("div", "lobby-code");
    this.codeText.dataset.lobby = "code";
    this.codeText.textContent = "------";
    this.copyButton = make("button", "screen-button lobby-copy");
    this.copyButton.type = "button";
    this.copyButton.dataset.screenAction = "copy-code";
    this.copyButton.textContent = "COPY";
    this.copyButton.disabled = true;
    this.copyButton.addEventListener("click", () => void this.copyCode());
    codeRow.append(codeLabel, this.codeText, this.copyButton);

    const joinRow = make("div", "lobby-join");
    this.joinInput = make("input", "settings-control lobby-input");
    this.joinInput.type = "text";
    this.joinInput.maxLength = 6;
    this.joinInput.autocomplete = "off";
    this.joinInput.spellcheck = false;
    this.joinInput.placeholder = "ABC123";
    this.joinInput.dataset.lobby = "join";
    this.joinInput.setAttribute("aria-label", "Room code");
    const join = make("button", "screen-button");
    join.type = "button";
    join.dataset.screenAction = "join-room";
    join.textContent = "JOIN";
    join.addEventListener("click", () => this.joinRoom());
    joinRow.append(this.joinInput, join);

    this.errorText = make("div", "lobby-error");
    this.errorText.dataset.lobby = "error";
    this.errorText.setAttribute("role", "alert");

    this.startButton = make("button", "screen-button screen-primary");
    this.startButton.type = "button";
    this.startButton.dataset.screenAction = "start-room";
    this.startButton.textContent = "START MATCH";
    this.startButton.disabled = true;
    this.startButton.addEventListener("click", () => {
      if (this.code !== "") {
        this.callbacks.onStart(this.code);
      }
    });

    const back = make("button", "screen-button screen-back");
    back.type = "button";
    back.dataset.screenAction = "lobby-back";
    back.textContent = "BACK";
    back.addEventListener("click", () => this.callbacks.onBack());

    this.element.append(title, create, codeRow, joinRow, this.errorText, this.startButton, back);
  }

  createRoom(): string {
    this.code = generateCode();
    this.showCode();
    this.errorText.textContent = "";
    return this.code;
  }

  joinRoom(): boolean {
    const value = this.joinInput.value.trim().toUpperCase();
    if (!validRoomCode(value)) {
      this.errorText.textContent = "Enter a 6-character room code";
      return false;
    }
    this.code = value;
    this.showCode();
    this.errorText.textContent = "";
    return true;
  }

  currentCode(): string {
    return this.code;
  }

  dispose(): void {
    this.element.remove();
  }

  private showCode(): void {
    this.codeText.textContent = this.code;
    this.copyButton.disabled = false;
    this.startButton.disabled = false;
  }

  private async copyCode(): Promise<void> {
    if (this.code === "") {
      return;
    }
    try {
      await navigator.clipboard.writeText(this.code);
      this.copyButton.textContent = "COPIED";
    } catch {
      // Clipboard access can be denied; the code is still readable and
      // selectable, so this is a hint failure, not a broken lobby.
      this.copyButton.textContent = "SELECT";
    }
    window.setTimeout(() => {
      this.copyButton.textContent = "COPY";
    }, 1200);
  }
}
