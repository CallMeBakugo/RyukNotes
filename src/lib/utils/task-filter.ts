export type TaskDueFilter = 'all' | 'today' | 'overdue';

/** Overdue means late and still open: a completed task is never overdue. */
export function matchesTaskDueFilter(due: string | null, filter: TaskDueFilter, today: string, completed = false): boolean {
  if (filter === 'all') return true;
  if (!due) return false;
  return filter === 'today' ? due === today : !completed && due < today;
}
