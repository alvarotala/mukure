import { test, type TestContext } from 'node:test';
import assert from 'node:assert/strict';
import { attachCompatibleVideo } from './compatible.ts';

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => { resolve = done; });
  return { promise, resolve };
}

function player(context: TestContext, options: {
  chunks: Uint8Array[];
  ranges?: number[][];
  position?: number;
  paused?: boolean;
  append?: (bytes: Uint8Array, state: { start: number; position: number; attempt: number }) => void;
}) {
  const ended = deferred<EndOfStreamError | undefined>();
  const errors: string[] = [];
  const accepted: number[] = [];
  const attempts: number[] = [];
  const removals: number[][] = [];
  let source: MockMediaSource;
  let ranges = options.ranges?.map((range) => [...range]) || [];
  class MockBuffer extends EventTarget {
    updating = false;
    get buffered() {
      return { length: ranges.length, start: (i: number) => ranges[i][0], end: (i: number) => ranges[i][1] };
    }
    complete() {
      this.updating = true;
      queueMicrotask(() => { this.updating = false; this.dispatchEvent(new Event('updateend')); });
    }
    appendBuffer(bytes: Uint8Array) {
      assert.equal(this.updating, false, 'append and remove must be serialized');
      attempts.push(bytes.length);
      options.append?.(bytes, { start: ranges[0]?.[0] ?? 0, position: video.currentTime, attempt: attempts.length });
      for (const byte of bytes) accepted.push(byte);
      this.complete();
    }
    remove(start: number, end: number) {
      assert.equal(this.updating, false, 'cannot evict during an append');
      removals.push([start, end]);
      ranges = ranges.filter((range) => range[1] > end).map((range) => [Math.max(range[0], end), range[1]]);
      this.complete();
    }
    abort() { this.updating = false; this.dispatchEvent(new Event('abort')); }
  }
  const buffer = new MockBuffer();
  class MockMediaSource extends EventTarget {
    readyState = 'closed';
    static isTypeSupported() { return true; }
    constructor() { super(); source = this; }
    addSourceBuffer() { return buffer; }
    endOfStream(error?: EndOfStreamError) { this.readyState = 'ended'; ended.resolve(error); }
  }
  class MockVideo extends EventTarget {
    src = '';
    currentTime = options.position || 0;
    paused = options.paused ?? false;
    error = null;
    load() {
      if (this.src) queueMicrotask(() => { source.readyState = 'open'; source.dispatchEvent(new Event('sourceopen')); });
    }
    pause() { this.paused = true; }
    getAttribute() { return this.src || null; }
    removeAttribute() { this.src = ''; }
  }
  const video = new MockVideo();
  const original = Object.getOwnPropertyDescriptor(globalThis, 'MediaSource');
  Object.defineProperty(globalThis, 'MediaSource', { configurable: true, writable: true, value: MockMediaSource });
  context.after(() => {
    if (original) Object.defineProperty(globalThis, 'MediaSource', original);
    else Reflect.deleteProperty(globalThis, 'MediaSource');
  });
  context.mock.method(URL, 'createObjectURL', () => 'blob:mukure-test');
  context.mock.method(URL, 'revokeObjectURL', () => {});
  context.mock.method(globalThis, 'fetch', async () => new Response(new ReadableStream<Uint8Array>({
    start(controller) { for (const bytes of options.chunks) controller.enqueue(bytes); controller.close(); },
  })));
  const detach = attachCompatibleVideo(video as unknown as HTMLVideoElement, 'http://127.0.0.1/fixture', (error) => errors.push(error));
  context.after(detach);
  return { ended: ended.promise, errors, accepted, attempts, removals, video, detach };
}

const quota = () => new DOMException('The quota has been exceeded.', 'QuotaExceededError');

test('quota recovery evicts played video and retries the same bytes without terminating playback', { timeout: 2000 }, async (context) => {
  const state = player(context, {
    chunks: [new Uint8Array([1, 2, 3]), new Uint8Array([4, 5])],
    ranges: [[0, 45]], position: 40,
    append: (_, state) => { if (state.start < 37) throw quota(); },
  });
  assert.equal(await state.ended, undefined);
  assert.deepEqual(state.errors, []);
  assert.deepEqual(state.accepted, [1, 2, 3, 4, 5]);
  assert.deepEqual(state.removals, [[0, 30], [0, 37]]);
  assert.deepEqual(state.attempts, [3, 3, 2]);
});

test('oversized append is split and every byte is accepted once, in order', { timeout: 2000 }, async (context) => {
  const input = Uint8Array.from({ length: 600000 }, (_, index) => index % 251);
  const state = player(context, {
    chunks: [input],
    append: (bytes) => { if (bytes.length > 64 * 1024) throw quota(); },
  });
  assert.equal(await state.ended, undefined);
  assert.deepEqual(state.errors, []);
  assert.deepEqual(new Uint8Array(state.accepted), input);
  assert.equal(Math.max(...state.attempts), 256 * 1024);
  assert.ok(state.attempts.includes(64 * 1024));
});

test('a full buffer waits for playback to free room before retrying pending bytes', { timeout: 2000 }, async (context) => {
  const waiting = deferred<void>();
  const state = player(context, {
    chunks: [new Uint8Array([7, 8, 9])], ranges: [[0, 1]],
    append: (_, state) => { if (state.position === 0) { waiting.resolve(); throw quota(); } },
  });
  await waiting.promise;
  await new Promise<void>((resolve) => setImmediate(resolve));
  assert.deepEqual(state.accepted, []);
  assert.deepEqual(state.errors, []);
  state.video.currentTime = 0.5;
  state.video.dispatchEvent(new Event('timeupdate'));
  assert.equal(await state.ended, undefined);
  assert.deepEqual(state.accepted, [7, 8, 9]);
  assert.equal(state.attempts.length, 2);
});

test('detaching cancels quota recovery while paused without reporting a playback error', { timeout: 2000 }, async (context) => {
  const waiting = deferred<void>();
  const state = player(context, {
    chunks: [new Uint8Array([10, 11])], ranges: [[0, 1]], paused: true,
    append: () => { waiting.resolve(); throw quota(); },
  });
  await waiting.promise;
  await new Promise<void>((resolve) => setImmediate(resolve));
  state.detach();
  await new Promise<void>((resolve) => setImmediate(resolve));
  assert.deepEqual(state.errors, []);
  assert.deepEqual(state.accepted, []);
  assert.equal(state.attempts.length, 1);
});

test('an unplayable full buffer reports a useful memory error instead of retrying forever', { timeout: 2000 }, async (context) => {
  const state = player(context, {
    chunks: [new Uint8Array([1])], append: () => { throw quota(); },
  });
  assert.equal(await state.ended, 'decode');
  assert.match(state.errors[0], /buffer de video.*liberar espacio/);
  assert.equal(state.attempts.length, 1);
});

test('non-quota append failures remain terminal and do not trigger retries', { timeout: 2000 }, async (context) => {
  const state = player(context, {
    chunks: [new Uint8Array([1])], append: () => { throw new Error('invalid fragment'); },
  });
  assert.equal(await state.ended, 'decode');
  assert.deepEqual(state.errors, ['invalid fragment']);
  assert.equal(state.attempts.length, 1);
});

test('quota recovery terminates if buffered playback remains stalled instead of looping indefinitely', { timeout: 2000 }, async (context) => {
  let clock = 0;
  context.mock.method(performance, 'now', () => { clock += 16000; return clock; });
  const waiting = deferred<void>();
  const state = player(context, {
    chunks: [new Uint8Array([1])], ranges: [[0, 1]],
    append: () => { waiting.resolve(); throw quota(); },
  });
  await waiting.promise;
  await new Promise<void>((resolve) => setImmediate(resolve));
  state.video.dispatchEvent(new Event('timeupdate'));
  assert.equal(await state.ended, 'decode');
  assert.match(state.errors[0], /buffer de video.*liberar espacio/);
  assert.equal(state.attempts.length, 1);
});
