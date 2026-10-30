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
