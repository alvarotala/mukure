export type PlaybackMode = 'direct' | 'compatible';

export interface AppInfo {
  cacheDir: string;
  ffmpegAvailable: boolean;
  ffmpegPath: string | null;
  version: string;
}

export interface VideoFile {
  id: number;
  name: string;
  length: number;
  mime: string;
}

export interface TorrentView {
  id: number;
  name: string;
  files: VideoFile[];
  subtitles: VideoFile[];
  selectedFile: number | null;
}

export interface TorrentStats {
  state: string;
  progressBytes: number;
  totalBytes: number;
  downloadSpeed: number;
  uploadSpeed: number;
  peers: number;
  finished: boolean;
}

export interface Playback {
  url: string;
  name: string;
  mode: string;
  duration: number | null;
}

export interface SubtitleCue {
  start: number;
  end: number;
  text: string;
}

export interface SubtitleTrack {
  name: string;
  cues: SubtitleCue[];
}

export interface EmbeddedSubtitleTrack {
  id: number;
  name: string;
  language: string;
  codec: string;
  supported: boolean;
}

export interface OnlineSubtitle {
  id: string;
  provider: string;
  name: string;
  language: string;
  url: string;
  format: string;
}

export interface SubtitleSearchResult {
  title: string;
  subtitles: OnlineSubtitle[];
}

export type SubtitleEvent =
  | { type: 'cues'; cues: SubtitleCue[] }
  | { type: 'done' }
  | { type: 'error'; message: string };

export interface CastDevice {
  id: string;
  name: string;
  model: string;
  address: string;
  port: number;
}
export interface CastStatus {
  active: boolean;
  deviceName: string;
  playerState: string;
  currentTime: number;
  duration: number | null;
  volume: number | null;
  muted: boolean;
  canSeek: boolean;
  error: string | null;
}
export type CastAction = 'play' | 'pause' | 'seek' | 'volume' | 'mute';
