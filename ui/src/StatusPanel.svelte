<script>
  import { clock } from './lib/format.js';

  let { tracker } = $props();

  const client = $derived(
    tracker.lcuConnected
      ? { state: 'ok', title: 'League client', detail: tracker.phase ? `connected · ${tracker.phase}` : 'connected' }
      : { state: 'idle', title: 'League client', detail: 'not running; games still get tracked' },
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
          return { state: 'warn', title: 'Reconnecting', detail: 'the match carries on where it left off' };
        }
        return {
          state: 'busy',
          title: 'Tracking your game',
          detail: current
            ? `${current.moments.length} moment${current.moments.length === 1 ? '' : 's'} · ${clock(current.gameTimeS)}`
            : '',
        };
      case 'Finalizing':
        return { state: 'busy', title: 'Match over', detail: 'saving the review' };
      default:
        return { state: 'ok', title: 'Ready', detail: 'your next game is recorded' };
    }
  });

  const capture = $derived.by(() => {
    const status = tracker.capture;
    switch (status?.state) {
      case undefined:
      case null:
        return null;
      case 'off':
        return { state: 'warn', title: 'Recording off', detail: '[capture] enabled = false' };
      case 'starting':
        return { state: 'busy', title: 'Getting ready to record', detail: 'waiting for the game window' };
      case 'recording':
        return { state: 'rec', title: 'Recording', detail: status.encoder ?? '' };
      case 'saving':
        return { state: 'busy', title: 'Saving the video', detail: '' };
      case 'failed':
        return { state: 'bad', title: 'Recording failed', detail: status.error ?? '' };
      default:
        return { state: 'ok', title: 'Recorder ready', detail: status?.encoder ? `with ${status.encoder}` : '' };
    }
  });
</script>

<ul class="status" aria-label="Status">
  {#each [game, capture, client].filter(Boolean) as row (row.title)}
    <li>
      <span class="dot {row.state}" aria-hidden="true"></span>
      <span class="text">
        <span class="title">{row.title}</span>
        {#if row.detail}<span class="detail">{row.detail}</span>{/if}
      </span>
    </li>
  {/each}
</ul>

<style>
  .status {
    list-style: none;
    margin: 0;
    padding: 0 6px;
    display: grid;
    gap: 12px;
  }
  li {
    display: flex;
    gap: 12px;
    align-items: flex-start;
  }
  .dot {
    flex: none;
    width: 10px;
    height: 10px;
    margin-top: 6px;
    border-radius: 50%;
    background: var(--muted);
  }
  .dot.ok {
    background: var(--win);
  }
  .dot.busy {
    background: var(--accent);
  }
  .dot.warn {
    background: var(--warn);
  }
  .dot.bad {
    background: var(--lose);
  }
  .dot.idle {
    background: var(--line);
  }
  .dot.rec {
    background: var(--fight);
    box-shadow: 0 0 0 4px color-mix(in srgb, var(--fight) 25%, transparent);
    animation: pulse 1.6s ease-in-out infinite;
  }
  @keyframes pulse {
    50% {
      opacity: 0.4;
    }
  }
  .text {
    display: grid;
    min-width: 0;
  }
  .title {
    font-size: 15px;
    font-weight: 600;
  }
  .detail {
    color: var(--muted);
    font-size: 13px;
    overflow-wrap: anywhere;
  }
</style>
