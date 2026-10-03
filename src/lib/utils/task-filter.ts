export type TaskDueFilter = 'all' | 'today' | 'overdue';

export function matchesTaskDueFilter(due: string | null, filter: TaskDueFilter, today: string): boolean {
  if (filter === 'all') return true;
  if (!due) return false;
  return filter === 'today' ? due === today : due < today;
}
