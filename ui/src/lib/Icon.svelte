<script>
  /** Line icons on a 24-unit grid, drawn for RiftReel. */
  let { name, size = 20 } = $props();
</script>

<svg
  class="icon"
  width={size}
  height={size}
  viewBox="0 0 24 24"
  fill="none"
  stroke="currentColor"
  stroke-width="2"
  stroke-linecap="round"
  stroke-linejoin="round"
  aria-hidden="true"
>
  {#if name === 'matches'}
    <rect x="3" y="4" width="18" height="12" rx="2.5" />
    <path d="M8 20h8M12 16v4" />
    <path d="M10.5 8.2v3.6l3.2-1.8z" fill="currentColor" />
  {:else if name === 'storage'}
    <rect x="3" y="5" width="18" height="14" rx="2.5" />
    <path d="M3 13h18" />
    <circle cx="16.5" cy="16" r="0.6" fill="currentColor" />
    <circle cx="13.5" cy="16" r="0.6" fill="currentColor" />
  {:else if name === 'back'}
    <path d="M15 5l-7 7 7 7" />
  {:else if name === 'play'}
    <path d="M8 5.5v13l10.5-6.5z" fill="currentColor" />
  {:else if name === 'scissors'}
    <circle cx="6" cy="7" r="2.6" />
    <circle cx="6" cy="17" r="2.6" />
    <path d="M8.2 8.6L20 18M8.2 15.4L20 6" />
  {:else if name === 'star'}
    <path d="M12 3.5l2.6 5.4 5.9.8-4.3 4.1 1 5.8-5.2-2.8-5.2 2.8 1-5.8-4.3-4.1 5.9-.8z" />
  {:else if name === 'star-filled'}
    <path
      d="M12 3.5l2.6 5.4 5.9.8-4.3 4.1 1 5.8-5.2-2.8-5.2 2.8 1-5.8-4.3-4.1 5.9-.8z"
      fill="currentColor"
    />
  {:else if name === 'folder'}
    <path d="M3 7.5A2.5 2.5 0 0 1 5.5 5H9l2 2h7.5A2.5 2.5 0 0 1 21 9.5v7a2.5 2.5 0 0 1-2.5 2.5h-13A2.5 2.5 0 0 1 3 16.5z" />
  {:else if name === 'copy'}
    <rect x="8.5" y="8.5" width="12" height="12" rx="2.5" />
    <path d="M15.5 8.5V6A2.5 2.5 0 0 0 13 3.5H6A2.5 2.5 0 0 0 3.5 6v7A2.5 2.5 0 0 0 6 15.5h2.5" />
  {:else if name === 'download'}
    <path d="M12 4v11M7 10.5l5 5 5-5M5 20h14" />
  {:else if name === 'trash'}
    <path d="M4 7h16M9.5 7V4.5h5V7M6.5 7l1 12.5h9l1-12.5M10 11v5M14 11v5" />
  {:else if name === 'check'}
    <path d="M5 12.5l4.5 4.5L19 7.5" />
  {:else if name === 'close'}
    <path d="M6 6l12 12M18 6L6 18" />
  {:else if name === 'import'}
    <path d="M12 3.5v11M7.5 10l4.5 4.5 4.5-4.5" />
    <path d="M4 14.5v3A2.5 2.5 0 0 0 6.5 20h11a2.5 2.5 0 0 0 2.5-2.5v-3" />
  {:else if name === 'sweep'}
    <path d="M4 20l7-7M9 9.5l5.5 5.5M13.5 5l5.5 5.5-4 4-5.5-5.5z" />
  {:else if name === 'flag'}
    <path d="M5 21V4M5 4h11l-2 4 2 4H5" />
  {/if}
</svg>

<style>
  .icon {
    flex: none;
    display: block;
  }
</style>
