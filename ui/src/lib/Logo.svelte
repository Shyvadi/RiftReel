<script>
  /** RiftReel's mark, as in src-tauri/icons/source.svg (both from tools/make-icons.py). */
  import { MARK } from './mark.js';

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
  {#each MARK as d}
    <path fill="#fff" fill-rule="evenodd" {d} />
  {/each}
</svg>
