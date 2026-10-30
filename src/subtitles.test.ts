import { test } from 'node:test';
import assert from 'node:assert/strict';
import { createSubtitleRenderer, shiftedCues } from './subtitles.ts';

test('a positive adjustment delays the original cue without changing its text', () => {
  const cues = [{ start: 0.25, end: 2.75, text: '<i>Hola</i>\nMundo' }];
  assert.deepEqual(shiftedCues(cues, 1.5), [{ start: 1.75, end: 4.25, text: '<i>Hola</i>\nMundo' }]);
  assert.equal(cues[0].start, 0.25);
});

test('negative adjustment clips cues crossing zero and drops elapsed cues', () => {
  assert.deepEqual(shiftedCues([
    { start: 0, end: 1, text: 'elapsed' },
    { start: 0.5, end: 2.5, text: 'crossing' },
    { start: 3, end: 4, text: 'later' },
  ], -1), [
    { start: 0, end: 1.5, text: 'crossing' },
    { start: 2, end: 3, text: 'later' },
  ]);
});

test('invalid offsets and intervals never reach VTTCue', () => {
  assert.throws(() => shiftedCues([], NaN));
  assert.throws(() => shiftedCues([{ start: 2, end: 1, text: 'invalid' }], 0));
  assert.throws(() => shiftedCues([{ start: 1, end: Infinity, text: 'invalid' }], 0));
});

test('native track replacement clears disabled cues, reuses one track and ignores stale cleanup', (context) => {
  // WebKit exposes track.cues=null while disabled. Model that behavior to
  // catch stale captions accumulating across visibility/offset changes.
  const originalCue = globalThis.VTTCue;
  class Cue {
    startTime: number;
    endTime: number;
    text: string;
    constructor(startTime: number, endTime: number, text: string) {
      this.startTime = startTime;
      this.endTime = endTime;
      this.text = text;
    }
  }
  globalThis.VTTCue = Cue as unknown as typeof VTTCue;
  context.after(() => { globalThis.VTTCue = originalCue; });
  const stored: VTTCue[] = [];
  let created = 0;
  const track = {
    mode: 'disabled',
    get cues() { return this.mode === 'disabled' ? null : stored; },
    addCue(cue: VTTCue) { stored.push(cue); },
    removeCue(cue: VTTCue) { stored.splice(stored.indexOf(cue), 1); },
  };
  const video = { addTextTrack() { ++created; return track; } } as unknown as HTMLVideoElement;
  const first = createSubtitleRenderer(video, 'first', 0, false);
  first.append([{ start: 0, end: 5, text: 'old' }]);
  assert.equal(track.cues, null);
  assert.equal(stored.length, 1);
  const second = createSubtitleRenderer(video, 'second', 1, true);
  second.append([{ start: 0, end: 5, text: 'new' }]);
  first.detach();
  first.append([{ start: 1, end: 3, text: 'stale' }]);
  assert.equal(created, 1);
  assert.deepEqual(stored.map((cue) => [cue.startTime, cue.endTime, cue.text]), [[1, 6, 'new']]);
  second.detach();
  assert.equal(stored.length, 0);
  assert.equal(track.mode, 'disabled');
});
