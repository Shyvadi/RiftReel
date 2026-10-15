<script>
  import { clock } from './lib/format.js';

  let { tracker } = $props();

  const marks = { ok: '✔', busy: '●', warn: '⚠', bad: '✖' };

  const client = $derived(
    tracker.lcuConnected
      ? { state: 'ok', title: 'League client detected', detail: tracker.phase ? `phase ${tracker.phase}` : '' }
      : {
          state: 'bad',
          title: 'League client not running',
          detail: 'games are still tracked from the game itself',
        },
  );

  const game = $derived.by(() => {
    const current = tracker.current;
    switch (tracker.state) {
      case 'Armed':
        return { state: 'busy', title: 'Champion select', detail: 'getting ready to track' };
      case 'InGame':
        if (current && !current.tracking) {
          const reason =
            current.skipped?.reason === 'spectator' ? 'spectator or replay' : `${current.skipped?.mode} is filtered out`;
          return { state: 'warn', title: 'Not tracking this game', detail: reason };
        }
        if (tracker.reconnecting) {
          return { state: 'warn', title: 'Reconnecting', detail: 'the match continues where it left off' };
        }
        return {
          state: 'busy',
          title: 'Tracking your game',
          detail: current
            ? `${current.moments.length} moment${current.moments.length === 1 ? '' : 's'} so far · ${clock(current.gameTimeS)}`
            : '',
        };
      case 'Finalizing':
        return { state: 'busy', title: 'Match over', detail: 'saving the review' };
      default:
        return { state: 'ok', title: 'Ready', detail: 'your next game will be tracked and reviewed here' };
    }
  });
</script>

<section class="status card" aria-label="Status">
  {#each [client, game] as row (row.title)}
    <div class="row">
      <span class="mark {row.state}" aria-hidden="true">{marks[row.state]}</span>
      <span class="title">{row.title}</span>
      {#if row.detail}<span class="detail">{row.detail}</span>{/if}
    </div>
  {/each}
</section>

<style>
  .status {
    display: grid;
    gap: 8px;
  }
  .row {
    display: flex;
    align-items: baseline;
    gap: 10px;
    flex-wrap: wrap;
  }
  .mark {
    width: 18px;
    text-align: center;
    font-size: 15px;
    font-family: 'Segoe UI Symbol', 'Segoe UI', system-ui, sans-serif;
  }
  .mark.ok {
    color: var(--win);
  }
  .mark.busy {
    color: var(--accent);
  }
  .mark.warn {
    color: var(--warn);
  }
  .mark.bad {
    color: var(--lose);
  }
  .title {
    font-weight: 600;
  }
  .detail {
    color: var(--muted);
  }
</style>
