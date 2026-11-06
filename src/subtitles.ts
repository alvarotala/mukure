import type { SubtitleCue, SubtitleTrack } from './types';

/** Positive offsets delay subtitles. Discard cues entirely before the video. */
export function shiftedCues(cues: SubtitleCue[], offset: number): SubtitleCue[] {
  if (!Number.isFinite(offset)) throw new Error('El ajuste de tiempo debe ser un número válido.');
  return cues.flatMap((cue) => {
    if (!Number.isFinite(cue.start) || !Number.isFinite(cue.end) || cue.end <= cue.start) {
      throw new Error('El archivo contiene un intervalo de subtítulos inválido.');
    }
    const end = cue.end + offset;
    return end <= 0 ? [] : [{ start: Math.max(0, cue.start + offset), end, text: cue.text }];
  });
}

export interface SubtitleRenderer {
  append: (cues: SubtitleCue[]) => void;
  detach: () => void;
}

interface NativeTrack {
  track: TextTrack;
  owner: symbol | null;
}

// Text tracks created by addTextTrack cannot be removed. Reuse exactly one per
// video element; the keyed video and its WeakMap entry are collected together.
const nativeTracks = new WeakMap<HTMLVideoElement, NativeTrack>();

function clearCues(track: TextTrack) {
  // A disabled text track may expose cues=null in WebKit.
  track.mode = 'hidden';
  if (track.cues) {
    for (const cue of Array.from(track.cues)) track.removeCue(cue);
  }
}

/** A native programmatic track works for both file playback and MediaSource. */
export function createSubtitleRenderer(video: HTMLVideoElement, _name: string, offset: number, enabled: boolean): SubtitleRenderer {
  let entry = nativeTracks.get(video);
  if (!entry) {
    entry = { track: video.addTextTrack('subtitles', 'Subtítulos de Mukure', 'und'), owner: null };
    nativeTracks.set(video, entry);
  }
  const { track } = entry;
  const owner = Symbol('subtitle-renderer');
  entry.owner = owner;
  clearCues(track);
  track.mode = enabled ? 'showing' : 'disabled';
  const detach = () => {
    if (entry.owner !== owner) return;
    clearCues(track);
    track.mode = 'disabled';
    entry.owner = null;
  };
  return {
    append(cues) {
      if (entry.owner !== owner) return;
      try {
        track.mode = 'hidden';
        for (const cue of shiftedCues(cues, offset)) {
          // VTTCue parses subtitle markup itself; text never enters the page's DOM.
          track.addCue(new VTTCue(cue.start, cue.end, cue.text));
        }
        track.mode = enabled ? 'showing' : 'disabled';
      } catch (error) {
        detach();
        throw error;
      }
    },
    detach,
  };
}

/** Use native text tracks so cues follow playback, seeking and native fullscreen. */
export function attachSubtitles(video: HTMLVideoElement, subtitles: SubtitleTrack, offset: number, enabled: boolean): () => void {
  const renderer = createSubtitleRenderer(video, subtitles.name, offset, enabled);
  renderer.append(subtitles.cues);
  return renderer.detach;
}

/** A snapshot for the Cast receiver, with the same timing adjustment as the Mac. */
export function subtitleWebVtt(track: SubtitleTrack, offset: number): string {
  const timestamp = (seconds: number) => {
    const ms = Math.round(seconds * 1000);
    return `${String(Math.floor(ms / 3600000)).padStart(2, '0')}:${String(Math.floor(ms / 60000) % 60).padStart(2, '0')}:${String(Math.floor(ms / 1000) % 60).padStart(2, '0')}.${String(ms % 1000).padStart(3, '0')}`;
  };
  return 'WEBVTT\n\n' + shiftedCues(track.cues, offset).map((cue, index) =>
    `${index + 1}\n${timestamp(cue.start)} --> ${timestamp(cue.end)}\n${cue.text.replace(/\n{2,}/g, '\n')}\n`).join('\n');
}
