<script>
  /** RiftReel's mark, as in src-tauri/icons/source.svg. */
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
    d="M324 263H541A172 172 0 0 1 627.3 583.8L741 761H583L484.2 607H410V761H324A49 49 0 0 1 275 712V312A49 49 0 0 1 324 263ZM430 378V492Q430 514 449 503L548 446Q567 435 548 424L449 367Q430 356 430 378Z"
  />
</svg>
