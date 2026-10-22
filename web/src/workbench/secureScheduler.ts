export interface ScheduledRecord { flow: object; priority: boolean }

/** Choose only each flow's FIFO head. A priority burst never excludes a waiting
 * ordinary head for more than four dispatches; least-recently-served flows rotate. */
export class SecureRecordScheduler {
  private priorityBurst = 0;
  private ticket = 0;
  private readonly served = new WeakMap<object, number>();
  choose<T extends ScheduledRecord>(queue: readonly T[]): number {
    const seen = new Set<object>();
    const ordinary: number[] = [], priority: number[] = [];
    for (let index = 0; index < queue.length; index++) {
      const item = queue[index];
      if (seen.has(item.flow)) continue;
      seen.add(item.flow);
      (item.priority ? priority : ordinary).push(index);
    }
    const candidates = priority.length && (this.priorityBurst < 4 || !ordinary.length) ? priority : ordinary;
    let selected = candidates[0] ?? -1;
    for (const index of candidates) if ((this.served.get(queue[index].flow) || 0) < (this.served.get(queue[selected].flow) || 0)) selected = index;
    if (selected !== -1) {
      this.priorityBurst = queue[selected].priority ? Math.min(4, this.priorityBurst + 1) : 0;
      this.served.set(queue[selected].flow, ++this.ticket);
    }
    return selected;
  }
}
