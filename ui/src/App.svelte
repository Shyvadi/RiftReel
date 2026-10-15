<script>
  import { onMount } from 'svelte';
  import { invoke } from '@tauri-apps/api/core';
  import { listen } from '@tauri-apps/api/event';

  let app = $state(null);
  let error = $state('');

  const stateLabels = {
    Idle: 'Idle',
    Armed: 'Champion select',
    InGame: 'In game',
    Finalizing: 'Match over',
  };

  function clock(seconds) {
    if (seconds == null) return '–';
    const tenths = Math.round(Math.abs(seconds) * 10);
    const minutes = String(Math.floor(tenths / 600)).padStart(2, '0');
    const secs = String(Math.floor((tenths % 600) / 10)).padStart(2, '0');
    return `${seconds < 0 ? '-' : ''}${minutes}:${secs}`;
  }

  const words = (label) => label.replaceAll('_', ' ');

  onMount(() => {
    let unlisten;
    invoke('get_status')
      .then((status) => (app = status))
      .catch((e) => (error = String(e)));
    listen('status', (event) => {
      if (app) app.tracker = event.payload;
    }).then((stop) => (unlisten = stop));
    return () => unlisten?.();
  });
</script>

{#snippet momentList(moments)}
  {#if moments.length}
    <ol class="moments">
      {#each moments as moment, i (i)}
        <li>
          <span class="time">{clock(moment.startS)}–{clock(moment.endS)}</span>
          <span class="label" data-label={moment.label}>{words(moment.label)}</span>
          <span class="count">{moment.markerIds.length} marker{moment.markerIds.length === 1 ? '' : 's'}</span>
        </li>
      {/each}
    </ol>
  {:else}
    <p class="muted">No moments yet.</p>
  {/if}
{/snippet}

<main>
  <header>
    <h1>Downplayed</h1>
    {#if app}<span class="muted">v{app.version}</span>{/if}
  </header>

  {#if error}
    <p class="error">{error}</p>
  {/if}

  {#if app}
    {@const tracker = app.tracker}
    <section class="pills">
      <span class="pill state">{stateLabels[tracker.state] ?? tracker.state}{tracker.reconnecting ? ' · reconnecting' : ''}</span>
      <span class="pill" class:ok={tracker.lcuConnected}>
        League client {tracker.lcuConnected ? 'connected' : 'not detected'}
      </span>
      {#if tracker.phase}<span class="pill muted">phase {tracker.phase}</span>{/if}
    </section>

    {#if tracker.current}
      {@const current = tracker.current}
      <section class="card">
        <h2>Current match</h2>
        {#if !current.tracking}
          <p class="muted">Not tracked ({current.skipped?.reason === 'spectator' ? 'spectator or replay' : `filtered: ${current.skipped?.mode}`}).</p>
        {:else}
          <p class="facts">
            <span>{current.champion ?? 'champion ?'}</span>
            <span>{current.gameMode ?? 'mode ?'}</span>
            <span>game time {clock(current.gameTimeS)}</span>
            <span>{current.markers} markers</span>
          </p>
          {@render momentList(current.moments)}
        {/if}
      </section>
    {/if}

    {#if tracker.lastMatch}
      {@const last = tracker.lastMatch}
      <section class="card">
        <h2>Last match</h2>
        <p class="facts">
          <span class="result" class:win={last.result === 'win'} class:lose={last.result === 'lose'}>
            {last.result ?? 'result unknown'}
          </span>
          <span>{last.champion ?? 'champion ?'}</span>
          <span>{last.queue ?? last.gameMode ?? ''}</span>
          {#if last.kda}<span>{last.kda.kills}/{last.kda.deaths}/{last.kda.assists}</span>{/if}
          {#if last.durationS}<span>{clock(last.durationS)}</span>{/if}
        </p>
        {@render momentList(last.moments)}
      </section>
    {:else if !tracker.current}
      <p class="muted empty">No match tracked yet. Play a game and its highlights show up here.</p>
    {/if}

    <footer class="muted">
      <div>Settings <code>{app.settingsFile}</code></div>
      <div>Logs <code>{app.logDir}</code></div>
      <div>Closing this window leaves Downplayed running in the tray.</div>
    </footer>
  {/if}
</main>

<style>
  :global(:root) {
    --bg: #f6f7f9;
    --card: #ffffff;
    --text: #1d2230;
    --muted: #6b7385;
    --line: #dde1e8;
    --accent: #0f9e8a;
    --win: #138a52;
    --lose: #b5373a;
    color-scheme: light dark;
    font-family: 'Segoe UI Variable', 'Segoe UI', system-ui, sans-serif;
    font-size: 14px;
  }
  @media (prefers-color-scheme: dark) {
    :global(:root) {
      --bg: #12151c;
      --card: #1b1f29;
      --text: #e7eaf0;
      --muted: #8a92a5;
      --line: #2b3140;
      --accent: #3fd1b9;
      --win: #4ccb8a;
      --lose: #f07073;
    }
  }
  :global(body) {
    margin: 0;
    background: var(--bg);
    color: var(--text);
  }
  main {
    max-width: 880px;
    margin: 0 auto;
    padding: 20px 24px 32px;
  }
  header {
    display: flex;
    align-items: baseline;
    gap: 10px;
  }
  h1 {
    margin: 0;
    font-size: 22px;
    letter-spacing: 0.2px;
  }
  h2 {
    margin: 0 0 10px;
    font-size: 15px;
  }
  .muted {
    color: var(--muted);
  }
  .error {
    color: var(--lose);
  }
  .pills {
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
    margin: 16px 0;
  }
  .pill {
    border: 1px solid var(--line);
    border-radius: 999px;
    padding: 3px 10px;
    background: var(--card);
  }
  .pill.state {
    border-color: var(--accent);
    color: var(--accent);
    font-weight: 600;
  }
  .pill.ok {
    border-color: var(--win);
  }
  .card {
    background: var(--card);
    border: 1px solid var(--line);
    border-radius: 10px;
    padding: 14px 16px;
    margin-bottom: 14px;
  }
  .facts {
    display: flex;
    flex-wrap: wrap;
    gap: 14px;
    margin: 0 0 10px;
  }
  .result {
    font-weight: 700;
    text-transform: uppercase;
  }
  .result.win {
    color: var(--win);
  }
  .result.lose {
    color: var(--lose);
  }
  .moments {
    list-style: none;
    margin: 0;
    padding: 0;
  }
  .moments li {
    display: grid;
    grid-template-columns: 120px 1fr auto;
    gap: 12px;
    padding: 6px 0;
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
  .label[data-label$='_kill']:not([data-label='turret_kill']):not([data-label='inhibitor_kill']):not([data-label='baron_kill']) {
    color: var(--accent);
  }
  .count {
    color: var(--muted);
  }
  .empty {
    margin: 24px 0;
  }
  footer {
    margin-top: 24px;
    display: grid;
    gap: 4px;
    font-size: 12px;
  }
  code {
    font-size: 12px;
    word-break: break-all;
  }
</style>
