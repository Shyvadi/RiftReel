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
    d="M517 430L572 417C617 435 657 465 697 490C727 505 752 513 787 513L832 575L782 583.6L767 611L752 588.8L647 607L802 625C777 655 737 675 697 685C637 703 577 715 557 715L417 725L287 800L377 675L252 645L362 595L242 540L347 505C297 475 237 435 192 375C267 385 347 405 397 440C372 365 327 285 282 220C377 250 467 325 502 420ZM757 535C767 529 777 529 787 535C777 541 767 541 757 535ZM542 477V538Q542 550 552.3 543.9L604.7 513.1Q615 507 604.6 501L552.4 471Q542 465 542 477Z"
  />
</svg>
