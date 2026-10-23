/**
 * The screen shell (task T-039).
 *
 * Boot → Main → Lobby → Match → Results, with Settings as a branch from Main.
 * One manager owns which screen is active and writes `body[data-screen]`; the
 * CSS uses that attribute to hide the canvas and the in-match UI whenever a
 * menu is up. Every screen has a control that leaves it, and the match has a
 * LEAVE button so Results is reachable without waiting for a core to fall.
 */

import "./screens.css";
import "./a11y.css";

export type ScreenId = "boot" | "main" | "lobby" | "settings" | "match" | "pause" | "results";

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

function actionButton(action: string, label: string, primary = false): HTMLButtonElement {
  const button = make("button", `screen-button${primary ? " screen-primary" : ""}`);
  button.type = "button";
  button.dataset.screenAction = action;
  button.textContent = label;
  return button;
}

export class ScreenManager {
  private readonly root: HTMLDivElement;
  private readonly menu: HTMLButtonElement;
  private readonly screens = new Map<ScreenId, HTMLElement>();
  private currentId: ScreenId = "boot";

  constructor(parent: HTMLElement, onMenu: () => void) {
    this.root = make("div", "screen-root");
    parent.append(this.root);

    this.menu = actionButton("pause", "MENU");
    this.menu.classList.add("match-menu");
    this.menu.addEventListener("click", onMenu);
    parent.append(this.menu);

    document.body.dataset.screen = "boot";
  }

  register(id: ScreenId, element: HTMLElement): void {
    // The panel is a direct child of the flex root; adding the `.screen`
    // wrapper class here would make its own children a row and push buttons
    // outside the panel's hit area.
    element.hidden = true;
    element.dataset.screen = id;
    this.screens.set(id, element);
    this.root.append(element);
  }

  show(id: ScreenId): void {
    this.currentId = id;
    document.body.dataset.screen = id;
    this.root.hidden = id === "match";
    for (const [screenId, element] of this.screens) {
      element.hidden = screenId !== id;
    }
  }

  current(): ScreenId {
    return this.currentId;
  }

  dispose(): void {
    this.root.remove();
    this.menu.remove();
    for (const element of this.screens.values()) {
      element.remove();
    }
    this.screens.clear();
  }
}

export function buildBoot(onContinue: () => void): HTMLDivElement {
  const panel = make("div", "screen-panel boot-panel");
  panel.dataset.screen = "boot";
  const title = make("h1", "boot-title");
  title.textContent = "BUDDY CRAFT";
  const subtitle = make("p", "screen-subtitle");
  subtitle.textContent = "Airdrops, crates and the recipes that fight back.";
  const continueButton = actionButton("continue", "CONTINUE", true);
  continueButton.addEventListener("click", onContinue);
  panel.append(title, subtitle, continueButton);
  return panel;
}

export interface MainMenuCallbacks {
  onOnline: () => void;
  onOffline: () => void;
  onSettings: () => void;
}

export function buildMainMenu(callbacks: MainMenuCallbacks): HTMLDivElement {
  const panel = make("div", "screen-panel main-panel");
  panel.dataset.screen = "main";
  const title = make("h1", "screen-title");
  title.textContent = "BUDDY CRAFT";
  const subtitle = make("p", "screen-subtitle");
  subtitle.textContent = "Choose how to play.";
  const online = actionButton("online", "ONLINE", true);
  online.addEventListener("click", callbacks.onOnline);
  const offline = actionButton("offline", "OFFLINE");
  offline.addEventListener("click", callbacks.onOffline);
  const settings = actionButton("settings", "SETTINGS");
  settings.addEventListener("click", callbacks.onSettings);
  panel.append(title, subtitle, online, offline, settings);
  return panel;
}

export interface PauseCallbacks {
  onResume: () => void;
  onSettings: () => void;
  onLeave: () => void;
}

export function buildPause(callbacks: PauseCallbacks): HTMLDivElement {
  const panel = make("div", "screen-panel pause-panel");
  panel.dataset.screen = "pause";
  panel.setAttribute("aria-label", "Paused");
  const title = make("h1", "screen-title");
  title.textContent = "PAUSED";
  const subtitle = make("p", "screen-subtitle");
  subtitle.textContent = "Offline matches wait here. Online matches keep running.";
  const resume = actionButton("pause-resume", "RESUME", true);
  resume.addEventListener("click", callbacks.onResume);
  const settings = actionButton("pause-settings", "SETTINGS");
  settings.addEventListener("click", callbacks.onSettings);
  const leave = actionButton("pause-leave", "LEAVE MATCH");
  leave.addEventListener("click", callbacks.onLeave);
  panel.append(title, subtitle, resume, settings, leave);
  return panel;
}

export interface ResultsCallbacks {
  onMain: () => void;
  onRematch: () => void;
}

export function buildResults(callbacks: ResultsCallbacks): HTMLDivElement {
  const panel = make("div", "screen-panel results-panel");
  panel.dataset.screen = "results";
  const title = make("h1", "screen-title");
  title.dataset.results = "title";
  title.textContent = "MATCH OVER";
  const subtitle = make("p", "screen-subtitle");
  subtitle.dataset.results = "detail";
  subtitle.textContent = "The arena is quiet. For now.";
  const rematch = actionButton("rematch", "REMATCH", true);
  rematch.addEventListener("click", callbacks.onRematch);
  const main = actionButton("results-main", "MAIN MENU");
  main.addEventListener("click", callbacks.onMain);
  panel.append(title, subtitle, rematch, main);
  return panel;
}
