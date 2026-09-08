import { expect, test } from 'vitest';
import { createQueueReader } from './queueReader';

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason: unknown) => void;
  const promise = new Promise<T>((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}

function harness() {
  const requests: ReturnType<typeof deferred<string[]>>[] = [];
  const snapshots: string[][] = [];
  const errors: unknown[] = [];
  const reader = createQueueReader(
    () => { const request = deferred<string[]>(); requests.push(request); return request.promise; },
    (items) => { snapshots.push(items); },
    (error) => { errors.push(error); },
  );
  return { reader, requests, snapshots, errors };
}

test('publishes a successful queue snapshot', async () => {
  const { reader, requests, snapshots } = harness();
  const refresh = reader.refresh();
  requests[0].resolve(['ready']);
  await refresh;
  expect(snapshots).toEqual([['ready']]);
});

test('an older response cannot overwrite a newer queue snapshot', async () => {
  const { reader, requests, snapshots } = harness();
  const older = reader.refresh();
  const newer = reader.refresh();
  requests[1].resolve(['completed']);
  await newer;
  requests[0].resolve(['processing']);
  await older;
  expect(snapshots).toEqual([['completed']]);
});

test('an obsolete snapshot is ignored even while the latest read is pending', async () => {
  const { reader, requests, snapshots } = harness();
  const older = reader.refresh();
  const newer = reader.refresh();
  requests[0].resolve(['processing']);
  await older;
  expect(snapshots).toEqual([]);
  requests[1].resolve(['ready']);
  await newer;
  expect(snapshots).toEqual([['ready']]);
});

test('a current failure is reported and rejects without erasing the last snapshot', async () => {
  const { reader, requests, snapshots, errors } = harness();
  const initial = reader.refresh();
  requests[0].resolve(['ready']);
  await initial;
  const error = new Error('Queue database is busy.');
  const refresh = reader.refresh();
  const rejected = expect(refresh).rejects.toBe(error);
  requests[1].reject(error);
  await rejected;
  expect(snapshots).toEqual([['ready']]);
  expect(errors).toEqual([error]);
});

test('an obsolete rejection cannot replace newer success with an error', async () => {
  const { reader, requests, snapshots, errors } = harness();
  const older = reader.refresh();
  const newer = reader.refresh();
  requests[1].resolve(['completed']);
  await newer;
  requests[0].reject(new Error('Old request failed.'));
  await older;
  expect(snapshots).toEqual([['completed']]);
  expect(errors).toEqual([]);
});

test('a later successful retry can publish after a current failure', async () => {
  const { reader, requests, snapshots } = harness();
  const failed = reader.refresh();
  const rejected = expect(failed).rejects.toThrow('Temporary failure.');
  requests[0].reject(new Error('Temporary failure.'));
  await rejected;
  const retry = reader.refresh();
  requests[1].resolve(['ready']);
  await retry;
  expect(snapshots).toEqual([['ready']]);
});

test('disposal suppresses late snapshots', async () => {
  const { reader, requests, snapshots } = harness();
  const refresh = reader.refresh();
  reader.dispose();
  requests[0].resolve(['ready']);
  await refresh;
  expect(snapshots).toEqual([]);
});

test('disposal absorbs late rejections rather than leaving an unhandled promise', async () => {
  const { reader, requests, errors } = harness();
  const refresh = reader.refresh();
  reader.dispose();
  requests[0].reject(new Error('Old bridge disconnected.'));
  await refresh;
  expect(errors).toEqual([]);
});

test('a disposed reader never starts another bridge request', async () => {
  const { reader, requests } = harness();
  reader.dispose();
  const refresh = reader.refresh();
  requests[0]?.resolve([]);
  await refresh;
  expect(requests).toHaveLength(0);
});

test('synchronous bridge errors use the same recoverable failure path', async () => {
  const error = new Error('Bridge unavailable.');
  const errors: unknown[] = [];
  const reader = createQueueReader(
    () => { throw error; },
    () => { throw new Error('No snapshot should be published.'); },
    (cause) => { errors.push(cause); },
  );
  await expect(reader.refresh()).rejects.toBe(error);
  expect(errors).toEqual([error]);
});
