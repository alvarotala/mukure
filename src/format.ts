export function formatBytes(value: number): string {
  if (!Number.isFinite(value) || value <= 0) return '0 B';
  const units = ['B', 'KB', 'MB', 'GB', 'TB'];
  const power = Math.min(Math.floor(Math.log(value) / Math.log(1024)), units.length - 1);
  return `${(value / 1024 ** power).toLocaleString('es', { maximumFractionDigits: power > 1 ? 1 : 0 })} ${units[power]}`;
}

export function fileName(value: string): string {
  return value.split(/[\\/]/).filter(Boolean).pop() || value;
}

export function errorMessage(error: unknown): string {
  if (error instanceof Error) return error.message;
  return typeof error === 'string' ? error : 'No se pudo completar la operación.';
}
