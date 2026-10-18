<script>
  import { onMount } from 'svelte';
  import { invoke } from '@tauri-apps/api/core';
  import { listen } from '@tauri-apps/api/event';
  import StatusPanel from './StatusPanel.svelte';
  import MatchList from './MatchList.svelte';
  import MatchView from './MatchView.svelte';
  import ClipsView from './ClipsView.svelte';
  import StorageView from './StorageView.svelte';
  import Icon from './lib/Icon.svelte';
  import Logo from './lib/Logo.svelte';
  import { clock, words } from './lib/format.js';

  let app = $state(null);
  let error = $state('');
  /** The match under review, or null for the page below. */
  let openId = $state(null);
  /** 'matches' (the list), 'clips' or 'storage'. */
  let page = $state('matches');
  const go = (next) => {
    openId = null;
    page = next;
  };
  /** Messages for the user (low disk, cleanup), shown until dismissed. */
  let notices = $state([]);
  const addNotice = (text) => {
    if (text && !notices.includes(text)) notices.push(text);
  };

  onMount(() => {
    const stops = [];
    invoke('get_status')
      .then((status) => {
        app = status;
        if (status.openMatch) openId = status.openMatch;
        addNotice(status.notice);
      })
      .catch((e) => (error = String(e)));
    listen('status', (event) => {
      if (app) app.tracker = event.payload;
    }).then((stop) => stops.push(stop));
    listen('open-match', (event) => (openId = event.payload)).then((stop) => stops.push(stop));
    listen('notice', (event) => addNotice(event.payload)).then((stop) => stops.push(stop));
    return () => stops.forEach((stop) => stop());
  });

  const pages = [
    { key: 'matches', label: 'Matches', icon: 'matches' },
    { key: 'clips', label: 'Clips', icon: 'scissors' },
    { key: 'storage', label: 'Storage', icon: 'storage' },
  ];
  /** A review's way back, named after the page it goes back to. */
  const backLabels = { matches: 'All matches', clips: 'All clips', storage: 'Storage' };
</script>

<div class="shell">
  <aside class="sidebar">
    <div class="brand">
      <Logo size={40} />
      <span>RiftReel</span>
    </div>
    <nav aria-label="Pages">
      {#each pages as item (item.key)}
        <button
          type="button"
          class:active={page === item.key && !openId}
          aria-current={page === item.key && !openId ? 'page' : undefined}
          onclick={() => go(item.key)}
        >
          <Icon name={item.icon} size={22} />
          {item.label}
        </button>
      {/each}
    </nav>
    {#if app}
      <div class="status">
        <StatusPanel tracker={app.tracker} />
      </div>
      <div class="version muted">RiftReel {app.version}</div>
    {/if}
  </aside>

  <main class="content">
    {#if error}<p class="error">{error}</p>{/if}
    {#each notices as notice (notice)}
      <div class="notice" role="status">
        <span>{notice}</span>
        <button
          type="button"
          class="icon-btn"
          aria-label="Dismiss"
          onclick={() => (notices = notices.filter((n) => n !== notice))}><Icon name="close" /></button
        >
      </div>
    {/each}

    {#if openId}
      <div class="page">
        <MatchView id={openId} backLabel={backLabels[page]} onBack={() => (openId = null)} />
      </div>
    {/if}
    {#if app && page === 'clips'}
      <div class="page" hidden={!!openId}>
        <ClipsView active={!openId} onOpen={(id) => (openId = id)} />
      </div>
    {/if}
    {#if app && page === 'storage'}
      <div class="page" hidden={!!openId}>
        <StorageView
          folders={{
            matches: app.matchesDir,
            recordings: app.recordingsDir,
            settings: app.settingsFile,
            logs: app.logDir,
          }}
          onOpen={(id) => (openId = id)}
        />
      </div>
    {/if}
    {#if app}
      {@const current = app.tracker.current}
      <!-- Kept while a review is open, so the list keeps its place. -->
      <div class="page matches-page" hidden={!!openId || page !== 'matches'}>
        {#if current?.tracking}
          <section class="card live">
            <div class="live-head">
              <span class="rec" aria-hidden="true"></span>
              <h2>Now playing: {current.champion ?? 'champion ?'}</h2>
              <span class="muted">{current.gameMode ?? ''} · {clock(current.gameTimeS)} · {current.markers} markers</span>
            </div>
            {#if current.moments.length}
              <ol class="moments">
                {#each current.moments as moment, i (i)}
                  <li>
                    <span class="time">{clock(moment.startS)}–{clock(moment.endS)}</span>
                    <span class="label">{words(moment.label)}</span>
                  </li>
                {/each}
              </ol>
            {/if}
          </section>
        {/if}
        <MatchList onOpen={(id) => (openId = id)} />
      </div>
    {/if}
  </main>
</div>

<style>
  :global(*, *::before, *::after) {
    box-sizing: border-box;
  }
  /* Red and black, like the icon: one dark theme, whatever Windows' own is. */
  :global(:root) {
    --bg: #0a0a0b;
    --sidebar: #101012;
    --card: #17171a;
    --raised: #1f1f23;
    --hover: #28282d;
    --text: #f1f1f3;
    --muted: #a3a3ad;
    --line: #2c2c31;
    --accent: #ef4444;
    --accent-hover: #f87171;
    --accent-soft: rgba(239, 68, 68, 0.16);
    --on-accent: #0a0a0b;
    --on-strong: #0a0a0b;
    --win: #34d399;
    --lose: #f87171;
    --warn: #fbbf24;
    --fight: #f87171;
    --objective: #a78bfa;
    --assist: #60a5fa;
    --death: #9ca3af;
    --radius: 14px;
    --radius-sm: 10px;
    --shadow: 0 1px 2px rgba(0, 0, 0, 0.4), 0 10px 28px rgba(0, 0, 0, 0.3);
    color-scheme: dark;
    font-family: 'Segoe UI Variable Text', 'Segoe UI', system-ui, sans-serif;
    font-size: 16px;
    line-height: 1.45;
  }
  :global(body) {
    margin: 0;
    background: var(--bg);
    color: var(--text);
    overflow: hidden;
  }
  :global([hidden]) {
    display: none !important;
  }
  :global(*) {
    scrollbar-color: color-mix(in srgb, var(--muted) 45%, transparent) transparent;
  }
  :global(:focus-visible) {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }
  :global(.muted) {
    color: var(--muted);
  }
  :global(.error) {
    color: var(--lose);
  }

  /* Cards and page titles. */
  :global(.card) {
    background: var(--card);
    border: 1px solid var(--line);
    border-radius: var(--radius);
    padding: 22px 26px;
    margin-bottom: 20px;
    box-shadow: var(--shadow);
  }
  :global(.card h2) {
    margin: 0 0 16px;
    font-size: 20px;
    line-height: 1.25;
  }
  :global(.card h3) {
    margin: 0 0 12px;
    font-size: 17px;
  }
  :global(.page-head) {
    display: flex;
    align-items: baseline;
    gap: 14px;
    margin: 4px 0 20px;
  }
  :global(.page-head h1) {
    margin: 0;
    font-size: 30px;
    line-height: 1.2;
    letter-spacing: -0.2px;
  }
  :global(.page-head .count) {
    color: var(--muted);
    font-size: 17px;
  }

  /* Buttons. */
  :global(.btn) {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    gap: 8px;
    height: 42px;
    padding: 0 18px;
    border-radius: var(--radius-sm);
    border: 1px solid var(--line);
    background: var(--raised);
    color: var(--text);
    font: inherit;
    font-size: 15px;
    font-weight: 600;
    cursor: pointer;
    white-space: nowrap;
    text-decoration: none;
  }
  :global(.btn:hover) {
    background: var(--hover);
  }
  :global(.btn:disabled) {
    opacity: 0.45;
    cursor: default;
  }
  :global(.btn.primary) {
    background: var(--accent);
    border-color: var(--accent);
    color: var(--on-accent);
  }
  :global(.btn.primary:hover:not(:disabled)) {
    background: var(--accent-hover);
    border-color: var(--accent-hover);
  }
  :global(.btn.ghost) {
    background: transparent;
    border-color: transparent;
  }
  :global(.btn.ghost:hover) {
    background: var(--hover);
  }
  :global(.btn.danger) {
    color: var(--lose);
  }
  :global(.btn.small) {
    height: 36px;
    padding: 0 14px;
    font-size: 14px;
  }
  :global(.icon-btn) {
    width: 40px;
    height: 40px;
    flex: none;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    padding: 0;
    border-radius: var(--radius-sm);
    border: 1px solid transparent;
    background: transparent;
    color: var(--muted);
    font: inherit;
    cursor: pointer;
  }
  :global(.icon-btn:hover:not(:disabled)) {
    background: var(--hover);
    color: var(--text);
  }
  :global(.icon-btn:disabled) {
    opacity: 0.35;
    cursor: default;
  }
  :global(.icon-btn.on) {
    color: var(--warn);
  }
  :global(.pill) {
    display: inline-flex;
    align-items: center;
    height: 26px;
    padding: 0 10px;
    border-radius: 999px;
    border: 1px solid var(--line);
    background: var(--raised);
    font-size: 13px;
    font-weight: 600;
    white-space: nowrap;
  }

  /* Fields. */
  :global(select),
  :global(input[type='text']),
  :global(input[type='number']),
  :global(input[type='date']) {
    height: 42px;
    padding: 0 12px;
    border-radius: var(--radius-sm);
    border: 1px solid var(--line);
    background: var(--raised);
    color: var(--text);
    font: inherit;
    font-size: 15px;
  }
  :global(select:hover),
  :global(input:hover) {
    border-color: var(--muted);
  }

  .shell {
    display: grid;
    grid-template-columns: 256px minmax(0, 1fr);
    height: 100vh;
  }
  .sidebar {
    display: flex;
    flex-direction: column;
    gap: 4px;
    min-height: 0;
    padding: 22px 16px 18px;
    background: var(--sidebar);
    border-right: 1px solid var(--line);
  }
  .brand {
    display: flex;
    align-items: center;
    gap: 12px;
    padding: 0 8px 26px;
    font-size: 21px;
    font-weight: 700;
    letter-spacing: 0.2px;
  }
  nav {
    display: grid;
    gap: 4px;
  }
  nav button {
    display: flex;
    align-items: center;
    gap: 14px;
    width: 100%;
    height: 48px;
    padding: 0 14px;
    border: 0;
    border-radius: var(--radius-sm);
    background: none;
    color: var(--muted);
    font: inherit;
    font-size: 16px;
    font-weight: 600;
    text-align: left;
    cursor: pointer;
  }
  nav button:hover {
    background: var(--hover);
    color: var(--text);
  }
  nav button.active {
    background: var(--accent-soft);
    color: var(--text);
  }
  nav button.active :global(.icon) {
    color: var(--accent);
  }
  .status {
    margin-top: auto;
    padding-top: 16px;
    border-top: 1px solid var(--line);
  }
  .version {
    padding: 10px 8px 0;
    font-size: 13px;
  }
  .content {
    min-width: 0;
    height: 100vh;
    overflow-y: auto;
    padding: 30px 40px 36px;
  }
  .page {
    max-width: 1320px;
    margin: 0 auto;
  }
  .matches-page {
    display: flex;
    flex-direction: column;
    height: 100%;
  }
  .notice {
    display: flex;
    align-items: center;
    gap: 14px;
    max-width: 1320px;
    margin: 0 auto 18px;
    padding: 10px 10px 10px 18px;
    border: 1px solid var(--warn);
    border-radius: var(--radius);
    background: var(--card);
    font-size: 15px;
  }
  .notice span {
    flex: 1;
  }
  .live {
    border-color: var(--accent);
  }
  .live-head {
    display: flex;
    flex-wrap: wrap;
    align-items: baseline;
    gap: 8px 14px;
  }
  .live-head h2 {
    margin: 0;
  }
  .rec {
    width: 12px;
    height: 12px;
    border-radius: 50%;
    background: var(--fight);
    align-self: center;
    animation: pulse 1.6s ease-in-out infinite;
  }
  @keyframes pulse {
    50% {
      opacity: 0.35;
    }
  }
  .moments {
    list-style: none;
    margin: 14px 0 0;
    padding: 0;
  }
  .moments li {
    display: grid;
    grid-template-columns: 140px 1fr;
    gap: 14px;
    padding: 8px 0;
    border-top: 1px solid var(--line);
  }
  .time {
    font-variant-numeric: tabular-nums;
    color: var(--muted);
  }
  .label {
    text-transform: capitalize;
    font-weight: 600;
  }
</style>
