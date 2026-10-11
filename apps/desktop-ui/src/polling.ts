export interface PollingClock {
  readonly setTimeout: (callback: () => void, delay: number) => number;
  readonly clearTimeout: (timer: number) => void;
}

export interface PollingOptions {
  readonly isVisible: () => boolean;
  readonly clock?: PollingClock;
  readonly intervalMs?: number;
  readonly hiddenIntervalMs?: number;
  readonly onError?: (error: unknown) => void;
}

/** Schedule after completion: slow requests cannot accumulate refresh work. */
export const startPolling = (task: () => Promise<void>, options: PollingOptions) => {
  const clock = options.clock ?? globalThis;
  const interval = options.intervalMs ?? 5_000;
  const hiddenInterval = options.hiddenIntervalMs ?? 30_000;
  let timer: number | undefined;
  let stopped = false;
  let running = false;

  const schedule = (delay: number) => {
    if (stopped) return;
    timer = clock.setTimeout(() => { timer = undefined; void run(); }, delay);
  };
  const run = async () => {
    if (stopped || running) return;
    if (!options.isVisible()) { schedule(hiddenInterval); return; }
    running = true;
    try { await task(); }
    catch (error) { options.onError?.(error); }
    finally {
      running = false;
      schedule(options.isVisible() ? interval : hiddenInterval);
    }
  };

  schedule(options.isVisible() ? interval : hiddenInterval);
  return {
    stop: () => {
      stopped = true;
      if (timer !== undefined) clock.clearTimeout(timer);
      timer = undefined;
    },
    wake: () => {
      if (stopped || running) return;
      if (timer !== undefined) clock.clearTimeout(timer);
      timer = undefined;
      schedule(options.isVisible() ? 0 : hiddenInterval);
    },
  };
};
