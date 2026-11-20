// Preferences, kept in the web view's storage. They only change how this
// phone behaves, so nothing here needs to be in the database.

export interface Settings {
  /** Preferred height: 360, 480, 720, 1080, 1440 or 2160. */
  quality: number;
  shorts: boolean;
  /** SponsorBlock categories to skip. */
  skip: string[];
  background: boolean;
  pip: boolean;
  /** Find streams for likely next videos ahead of time. */
  prefetch: boolean;
  /** Play a related video when one ends and the queue is empty. */
  autoplay: boolean;
  /** Height for downloads. */
  downloadQuality: number;
  /** Phones: notify about new videos from your subscriptions. */
  newVideos: boolean;
  /** Android was asked whether Vista may notify (it asks once). */
  askedNotifications: boolean;
  /** Download what's in Watch later, on Wi-Fi. */
  downloadLater: boolean;
  /** The newest version already offered on launch (offered once each). */
  updateOffered: string;
  /** Save a backup by itself once a week. */
  autoBackup: boolean;
  /** When the last automatic backup was made (seconds since 1970). */
  lastAutoBackup: number;
}

export const SKIP_CATEGORIES: { id: string; label: string; hint: string }[] = [
  { id: "sponsor", label: "Sponsors", hint: "Paid promotions" },
  { id: "selfpromo", label: "Self-promotion", hint: "Merch, Patreon, their other channels" },
  { id: "interaction", label: "Subscribe reminders", hint: "\"Like and subscribe\"" },
  { id: "intro", label: "Intros", hint: "Intro animations" },
  { id: "outro", label: "Endcards", hint: "Credits and end screens" },
  { id: "preview", label: "Previews", hint: "\"Coming up\" recaps" },
  { id: "music_offtopic", label: "Non-music parts", hint: "In music videos" },
  { id: "filler", label: "Filler", hint: "Tangents and jokes" },
];

const DEFAULTS: Settings = {
  quality: 1080,
  shorts: false,
  skip: ["sponsor", "selfpromo", "interaction"],
  background: true,
  pip: true,
  prefetch: true,
  autoplay: false,
  downloadQuality: 720,
  newVideos: true,
  askedNotifications: false,
  downloadLater: false,
  updateOffered: "",
  autoBackup: true,
  lastAutoBackup: 0,
};

const KEY = "vista-settings";

function load(): Settings {
  try {
    return { ...DEFAULTS, ...JSON.parse(localStorage.getItem(KEY) ?? "{}") };
  } catch {
    return { ...DEFAULTS };
  }
}

export const settings: Settings = load();

export function save(change: Partial<Settings>) {
  Object.assign(settings, change);
  try {
    localStorage.setItem(KEY, JSON.stringify(settings));
  } catch {
    // Storage unavailable; the change still holds until the app closes.
  }
}
