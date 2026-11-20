// Typed wrappers around the Rust commands (src-tauri/src/commands.rs).
import { invoke } from "@tauri-apps/api/core";

export interface Video {
  id: string;
  title: string;
  channel: string;
  channelId?: string | null;
  duration?: number | null;
  /** "5.8M views" */
  views?: string | null;
  /** "3 days ago" */
  published?: string | null;
  /** Seconds since 1970 (feed entries). */
  publishedAt?: number | null;
  thumbnail: string;
  avatar?: string | null;
  live?: boolean;
}

export interface Channel {
  id: string;
  name: string;
  avatar?: string | null;
  handle?: string | null;
  subscribers?: string | null;
}

export interface Page<T> {
  items: T[];
  next: string | null;
}

export interface Playlist {
  id: string;
  title: string;
  channel: string;
  /** "29 videos" */
  count?: string | null;
  thumbnail?: string | null;
}

export interface SearchResults {
  channels: Channel[];
  playlists: Playlist[];
  videos: Video[];
  next: string | null;
}

export interface WatchPage {
  id: string;
  title: string;
  views: string | null;
  date: string | null;
  likes: string | null;
  description: string;
  channel: Channel;
  related: Page<Video>;
  /** Token for the first part of the comments; null when they're turned off. */
  comments: string | null;
  subscribed: boolean;
  saved: boolean;
}

export interface ChannelView {
  channel: Channel;
  description: string;
  videos: Page<Video>;
  subscribed: boolean;
}

export interface Chapter {
  start: number;
  title: string;
}

export interface Segment {
  start: number;
  end: number;
  category: string;
}

export interface PlayInfo {
  qualities: number[];
  quality: number | null;
  duration: number | null;
  chapters: Chapter[];
  segments: Segment[];
  live: boolean;
  /** Desktop: a media:// path for the <video> element. */
  src: string | null;
  /** Playing a download. */
  offline: boolean;
}

export interface Comment {
  id: string;
  author: string;
  authorId: string | null;
  avatar: string | null;
  text: string;
  published: string;
  likes: string | null;
  replyCount: string | null;
  /** Token for this comment's replies. */
  replies: string | null;
  creator: boolean;
}

export interface Comments {
  count: string | null;
  items: Comment[];
  next: string | null;
}

export interface PlaylistView {
  playlist: Playlist;
  videos: Page<Video>;
  saved: boolean;
}

export interface Link {
  kind: "video" | "channel" | "playlist";
  id: string;
  start: number | null;
}

export interface DownloadItem {
  video: Video;
  quality: number;
  size: number;
  done: boolean;
  error: string | null;
  /** Being downloaded right now. */
  active: boolean;
  /** Its thumbnail is saved (media://thumb/<id>). */
  thumb: boolean;
  /** Finished, but the files are gone. */
  missing: boolean;
}

export interface Feed {
  videos: Video[];
  checked: number | null;
  failed: number;
}

export interface Watched {
  video: Video;
  position: number;
  duration: number | null;
  watched: number;
}

/** A video or channel kept out of recommendations (db.rs). */
export interface Hidden {
  id: string;
  kind: "video" | "channel";
  name: string;
}

/** A newer Vista on GitHub (updater.rs). */
export interface AppRelease {
  version: string;
  notes: string;
  size: number;
}

/** What a backup holds (backup.rs). */
export interface BackupSummary {
  createdAt: number;
  appVersion: string;
  subscriptions: number;
  history: number;
  later: number;
  playlists: number;
}

export interface Votes {
  likes: number;
  dislikes: number;
}

export interface YtdlpStatus {
  installed: boolean;
  managed: boolean;
  version: string | null;
}

export type PlayerCommand = "play" | "pause" | "seek" | "rate" | "show" | "hide" | "stop" | "background" | "pip" | "fullscreen" | "sleep";

export const api = {
  ytdlpStatus: () => invoke<YtdlpStatus>("ytdlp_status"),
  ytdlpInstall: () => invoke<YtdlpStatus>("ytdlp_install"),
  search: (query: string, next?: string) => invoke<SearchResults>("search", { query, next }),
  watchInfo: (id: string) => invoke<WatchPage>("watch_info", { id }),
  more: (kind: "related" | "channel", token: string, channel?: Channel) => invoke<Page<Video>>("more", { kind, token, channel }),
  channel: (id: string) => invoke<ChannelView>("channel", { id }),
  play: (video: Video, quality: number, start: number, skip: string[], fresh = false) =>
    invoke<PlayInfo>("play", { video, quality, start, skip, fresh }),
  prefetch: (ids: string[]) => invoke<void>("prefetch", { ids }),
  playerCommand: (command: PlayerCommand, value?: number) => invoke<void>("player_command", { command, value }),
  uploadsWatch: (json: string, on: boolean) => invoke<void>("uploads_watch", { json, on }),
  notificationsAllow: () => invoke<boolean>("notifications_allow"),
  /** Wi-Fi, or another connection that isn't metered (always true on the desktop). */
  unmetered: () => invoke<boolean>("unmetered"),
  /** Opens Android's share sheet; false on the desktop, which has none. */
  share: (id: string, start?: number) => invoke<boolean>("share", { id, start }),
  appUpdateCheck: () => invoke<{ current: string; update: AppRelease | null }>("app_update_check"),
  /** Downloads it and opens Android's installer; false when Android first wants Vista allowed to install apps. */
  appUpdateInstall: () => invoke<boolean>("app_update_install"),
  backupSave: (settings: string, name: string, auto: boolean) => invoke<string>("backup_save", { settings, name, auto }),
  /** Hands Rust a picked backup file; returns where it keeps it. */
  backupStage: (bytes: ArrayBuffer) => invoke<string>("backup_stage", { bytes: Array.from(new Uint8Array(bytes)) }),
  backupPeek: (path: string) => invoke<BackupSummary>("backup_peek", { path }),
  backupRestore: (path: string, currentSettings: string) =>
    invoke<{ summary: BackupSummary; settings: string }>("backup_restore", { path, currentSettings }),
  votes: (id: string) => invoke<Votes>("votes", { id }),
  comments: (token: string) => invoke<Comments>("comments", { token }),
  playlist: (id: string) => invoke<PlaylistView>("playlist", { id }),
  playlistSave: (playlist: Playlist, saved: boolean) => invoke<void>("playlist_save", { playlist, saved }),
  savedPlaylists: () => invoke<Playlist[]>("saved_playlists"),
  resolveLink: (text: string) => invoke<Link>("resolve_link", { text }),
  downloads: () => invoke<DownloadItem[]>("downloads_list"),
  downloadAdd: (video: Video, quality: number) => invoke<void>("download_add", { video, quality }),
  downloadRemove: (id: string) => invoke<void>("download_remove", { id }),
  feed: (refresh: boolean, shorts: boolean) => invoke<Feed>("feed", { refresh, shorts }),
  recommendations: (refresh: boolean) =>
    invoke<{ items: { video: Video; reason: string }[]; made: number | null }>("recommendations", { refresh }),
  topic: (topic: string) => invoke<{ video: Video; reason: string }[]>("topic", { topic }),
  searchSuggestions: (query: string) => invoke<string[]>("search_suggestions", { query }),
  searches: () => invoke<string[]>("searches"),
  searchRemove: (query: string) => invoke<void>("search_remove", { query }),
  searchesClear: () => invoke<void>("searches_clear"),
  subscriptions: () => invoke<Channel[]>("subscriptions"),
  subscribe: (channel: Channel, on: boolean) => invoke<void>("subscribe", { channel, on }),
  importSubscriptions: (csv: string) => invoke<{ found: number; added: number }>("import_subscriptions", { csv }),
  history: (add?: Video) => invoke<Watched[]>("history", { add }),
  historyProgress: (id: string, position: number, duration: number | null) =>
    invoke<void>("history_progress", { id, position, duration }),
  historyRemove: (id?: string) => invoke<void>("history_remove", { id }),
  positions: (ids: string[]) => invoke<Record<string, { position: number; duration: number | null }>>("positions", { ids }),
  hidden: () => invoke<Hidden[]>("hidden"),
  hide: (item: Hidden, on: boolean) => invoke<void>("hide", { item, on }),
  later: () => invoke<Video[]>("later"),
  laterSet: (video: Video, saved: boolean) => invoke<void>("later_set", { video, saved }),
};

/** Errors from commands arrive as plain strings. */
export function message(e: unknown): string {
  return typeof e === "string" ? e : e instanceof Error ? e.message : String(e);
}
