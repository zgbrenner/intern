export interface QueueReader {
  refresh(): Promise<void>;
  dispose(): void;
}

/**
 * Only the most recently requested snapshot may update the queue. Desktop
 * commands and queue events can overlap, and their replies need not arrive in
 * order. A reader belongs to one subscription lifetime, never to a new bridge
 * or a remounted effect. Disposing it also absorbs obsolete rejections.
 */
export function createQueueReader<T>(
  load: () => Promise<T>,
  publish: (snapshot: T) => void,
  fail: (error: unknown) => void,
): QueueReader {
  let active = true;
  let revision = 0;
  return {
    async refresh() {
      if (!active) return;
      const request = ++revision;
      try {
        const snapshot = await load();
        if (active && request === revision) publish(snapshot);
      } catch (error) {
        if (!active || request !== revision) return;
        fail(error);
        throw error;
      }
    },
    dispose() { active = false; revision += 1; },
  };
}
