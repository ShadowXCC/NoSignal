<script lang="ts">
  // "Keep changes?" bar for a pending change, GNOME-resolution-dialog style.
  import { status } from "$lib/stores";
  import { api } from "$lib/api";
  import { refresh } from "$lib/stores";

  let remaining = $state(0);
  let timer: ReturnType<typeof setInterval> | null = null;

  $effect(() => {
    const pending = $status?.pending ?? null;
    if (pending) {
      remaining = pending.deadline_secs;
      timer ??= setInterval(() => {
        remaining = Math.max(0, remaining - 1);
      }, 1000);
    } else if (timer) {
      clearInterval(timer);
      timer = null;
    }
  });

  async function keep() {
    await api.confirmPending();
    await refresh();
  }
  async function revert() {
    await api.revertPending();
    await refresh();
  }
</script>

{#if $status?.pending}
  <div class="pending">
    <strong>Keep these display changes?</strong>
    <span>Reverting automatically in {remaining}s…</span>
    <span class="grow"></span>
    <button onclick={revert}>Revert now</button>
    <button class="primary" onclick={keep}>Keep changes</button>
  </div>
{/if}

<style>
  .pending {
    display: flex;
    align-items: center;
    gap: 0.75rem;
    padding: 0.7rem 1rem;
    border-radius: 8px;
    margin-bottom: 1rem;
    background: color-mix(in srgb, var(--accent) 12%, transparent);
    border: 1px solid color-mix(in srgb, var(--accent) 45%, transparent);
  }
  .grow {
    flex: 1;
  }
</style>
