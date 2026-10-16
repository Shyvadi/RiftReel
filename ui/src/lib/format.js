// Formatting shared by the views.

/** Game seconds as `mm:ss`. */
export function clock(seconds) {
  if (seconds == null) return '–';
  const whole = Math.round(Math.abs(seconds));
  const minutes = String(Math.floor(whole / 60)).padStart(2, '0');
  const secs = String(whole % 60).padStart(2, '0');
  return `${seconds < 0 ? '-' : ''}${minutes}:${secs}`;
}

/** `double_kill` → `double kill`. */
export const words = (label) => (label ?? '').replaceAll('_', ' ');

/** Colour group of a marker or moment label. */
export function category(label) {
  switch (label) {
    case 'victory':
      return 'win';
    case 'clip':
      return 'custom';
    case 'death':
      return 'death';
    case 'assist':
      return 'assist';
    case 'baron_kill':
    case 'elder_dragon':
    case 'dragon_kill':
    case 'rift_herald':
    case 'voidgrubs':
    case 'inhibitor_kill':
    case 'turret_kill':
      return 'objective';
    default:
      return 'fight';
  }
}

export function kda(value) {
  if (!value) return '–';
  return `${value.kills} / ${value.deaths} / ${value.assists}`;
}

export function when(ms) {
  if (!ms) return '';
  return new Date(ms).toLocaleString(undefined, { dateStyle: 'medium', timeStyle: 'short' });
}
