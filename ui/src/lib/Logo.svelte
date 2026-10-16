<script>
  /** Downplayed's mark, as in src-tauri/icons/source.svg. */
  let { size = 36 } = $props();
  const gradient = `logo-${Math.random().toString(36).slice(2)}`;
</script>

<svg width={size} height={size} viewBox="48 48 928 928" aria-hidden="true">
  <defs>
    <linearGradient id={gradient} x1="0" y1="0" x2="1" y2="1">
      <stop offset="0" stop-color="#14b8a6" />
      <stop offset="1" stop-color="#2563eb" />
    </linearGradient>
  </defs>
  <rect x="48" y="48" width="928" height="928" rx="208" fill="url(#{gradient})" />
  <path
    fill="#fff"
    fill-rule="evenodd"
    d="M324 263H492A249 249 0 0 1 492 761H324A49 49 0 0 1 275 712V312A49 49 0 0 1 324 263ZM407 416V608Q407 633 429 621L597 524Q618 512 597 500L429 403Q407 391 407 416Z"
  />
</svg>
