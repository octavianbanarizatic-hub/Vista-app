// One interface over the two players: the native one on Android (laid over
// the watch page's player slot, reporting through window.__vistaPlayer) and
// a <video> element on desktop, which is only there for testing the app.
import { convertFileSrc } from "@tauri-apps/api/core";
import { api, type PlayInfo, type Segment, type Video } from "./api";
import { settings } from "./settings";

export const isAndroid = /Android/i.test(navigator.userAgent);

export interface PlayerState {
  playing: boolean;
  buffering: boolean;
  ended: boolean;
  position: number;
  duration: number | null;
  rate: number;
}

export type PlayerEvent =
  | { type: "state"; state: PlayerState }
  | { type: "time"; position: number; duration: number | null }
  | { type: "skip"; category: string }
  | { type: "error"; message: string; code?: string; position: number }
  | { type: "fullscreen"; on: boolean }
  | { type: "pip"; on: boolean };

type Listener = (e: PlayerEvent) => void;

class Player {
  state: PlayerState = { playing: false, buffering: false, ended: false, position: 0, duration: null, rate: 1 };
  private listeners: Listener[] = [];
  private el: HTMLVideoElement | null = null;
  private segments: Segment[] = [];
  private lastPosition = 0;

  constructor() {
    if (isAndroid) {
      (window as any).__vistaPlayer = (type: string, data: any) => {
        if (type === "state") {
          this.state = data;
          this.emit({ type: "state", state: data });
        } else if (type === "time") {
          this.state.position = data.position;
          this.state.duration = data.duration;
          this.emit({ type: "time", position: data.position, duration: data.duration });
        } else {
          this.emit({ type, ...data } as PlayerEvent);
        }
      };
    }
  }

  on(listener: Listener) {
    this.listeners.push(listener);
  }

  private emit(e: PlayerEvent) {
    this.listeners.forEach((l) => l(e));
  }

  /** Resolves the video's streams and starts it. Throws "superseded" when another video was started meanwhile. */
  async start(video: Video, quality: number, start: number, slot: HTMLElement, fresh = false): Promise<PlayInfo> {
    const info = await api.play(video, quality, start, settings.skip, fresh);
    this.state = { ...this.state, position: start, duration: info.duration, ended: false };
    if (!isAndroid && info.src) this.startDesktop(info, start, slot);
    return info;
  }

  private startDesktop(info: PlayInfo, start: number, slot: HTMLElement) {
    if (!this.el) {
      const el = document.createElement("video");
      el.controls = true;
      el.playsInline = true;
      const report = () => {
        this.state = {
          playing: !el.paused && !el.ended,
          buffering: el.readyState < 3 && !el.paused,
          ended: el.ended,
          position: el.currentTime,
          duration: Number.isFinite(el.duration) ? el.duration : null,
          rate: el.playbackRate,
        };
        this.emit({ type: "state", state: this.state });
      };
      ["play", "pause", "ended", "waiting", "playing", "ratechange", "loadedmetadata"].forEach((t) => el.addEventListener(t, report));
      el.addEventListener("timeupdate", () => {
        const pos = el.currentTime;
        // Skip as playback reaches a segment, not when seeking into one (as on Android).
        const hit = this.segments.find((s) => this.lastPosition < s.start + 0.5 && pos >= s.start && pos < s.end - 0.5);
        this.lastPosition = pos;
        if (hit && !el.paused) {
          el.currentTime = hit.end;
          this.lastPosition = hit.end;
          this.emit({ type: "skip", category: hit.category });
        }
        this.state.position = pos;
        this.emit({ type: "time", position: pos, duration: this.state.duration });
      });
      el.addEventListener("error", () => {
        this.emit({ type: "error", message: el.error?.message || "The video couldn't be played.", position: el.currentTime });
      });
      this.el = el;
    }
    if (this.el.parentElement !== slot) slot.append(this.el);
    this.segments = info.segments;
    this.lastPosition = start;
    this.el.src = convertFileSrc(info.src!, "media");
    this.el.currentTime = start;
    void this.el.play().catch(() => {});
  }

  play() {
    if (this.el) void this.el.play().catch(() => {});
    else void api.playerCommand("play");
  }

  pause() {
    if (this.el) this.el.pause();
    else void api.playerCommand("pause");
  }

  toggle() {
    if (this.state.playing) this.pause();
    else this.play();
  }

  seek(seconds: number) {
    this.lastPosition = seconds;
    if (this.el) this.el.currentTime = seconds;
    else void api.playerCommand("seek", seconds);
  }

  setRate(rate: number) {
    this.state.rate = rate;
    if (this.el) this.el.playbackRate = rate;
    else void api.playerCommand("rate", rate);
  }

  private sleepTimer = 0;

  /** Pauses in `seconds` (0 cancels). On phones the native player keeps the
   *  time, since the page's timers stop while it sleeps in the background. */
  sleep(seconds: number) {
    if (isAndroid) return void api.playerCommand("sleep", seconds);
    clearTimeout(this.sleepTimer);
    if (seconds > 0) this.sleepTimer = window.setTimeout(() => this.pause(), seconds * 1000);
  }

  /** Android: shows or hides the native player over the page. */
  setVisible(on: boolean) {
    if (isAndroid) void api.playerCommand(on ? "show" : "hide");
  }

  stop() {
    if (this.el) {
      this.el.pause();
      this.el.removeAttribute("src");
      this.el.load();
      this.el.remove();
    } else if (isAndroid) {
      void api.playerCommand("stop");
    }
    this.segments = [];
    this.state = { playing: false, buffering: false, ended: false, position: 0, duration: null, rate: this.state.rate };
    this.emit({ type: "state", state: this.state });
  }

  /** Passes the background-play and picture-in-picture settings to the phone. */
  applySettings() {
    if (!isAndroid) return;
    void api.playerCommand("background", settings.background ? 1 : 0);
    void api.playerCommand("pip", settings.pip ? 1 : 0);
  }
}

export const player = new Player();
