<script lang="ts">
  // Displays view: spatial canvas mirroring the physical arrangement.
  // Disabled outputs are ghosted on a shelf below (the server forgets their
  // position; the daemon remembers and restores it on enable).
  import { topology, profiles, refresh } from "$lib/stores";
  import { api, modeLabel, outputLabel, type Output } from "$lib/api";

  let selected = $state<string | null>(null);
  let aliasDraft = $state("");
  let busy = $state(false);
  let notice = $state<string | null>(null);

  const outputs = $derived($topology?.outputs ?? []);
  const enabled = $derived(outputs.filter((o) => o.enabled));
  const disabled = $derived(outputs.filter((o) => !o.enabled));
  const selectedOutput = $derived(
    outputs.find((o) => o.identity.connector === selected) ?? null,
  );
  const firstRun = $derived(
    ($profiles?.profiles.length ?? 0) === 0 && outputs.length > 0,
  );

  // Canvas geometry: fit enabled outputs into the viewbox.
  const CANVAS_W = 640;
  const CANVAS_H = 340;
  function logicalSize(o: Output): [number, number] {
    const w = o.mode ? o.mode.width / (o.scale || 1) : 800;
    const h = o.mode ? o.mode.height / (o.scale || 1) : 450;
    const rotated = o.transform.includes("90") || o.transform.includes("270");
    return rotated ? [h, w] : [w, h];
  }
  const canvasRects = $derived.by(() => {
    if (enabled.length === 0) return [];
    let maxX = 1,
      maxY = 1;
    for (const o of enabled) {
      const [w, h] = logicalSize(o);
      maxX = Math.max(maxX, o.position[0] + w);
      maxY = Math.max(maxY, o.position[1] + h);
    }
    const scale = Math.min((CANVAS_W - 20) / maxX, (CANVAS_H - 20) / maxY);
    return enabled.map((o) => {
      const [w, h] = logicalSize(o);
      return {
        output: o,
        x: 10 + o.position[0] * scale,
        y: 10 + o.position[1] * scale,
        w: w * scale,
        h: h * scale,
      };
    });
  });

  async function toggle(o: Output) {
    if (busy) return;
    busy = true;
    notice = null;
    try {
      const target = o.identity.connector;
      const mode = o.enabled ? "off" : "on";
      let result = await api.setOutput(target, mode, false, null);
      if (
        result.outcome === "guard_refused" &&
        result.reason.includes("force") &&
        confirm(`${result.reason}\n\nProceed anyway?`)
      ) {
        result = await api.setOutput(target, mode, true, null);
      }
      if (result.outcome === "guard_refused") {
        notice = result.reason;
      }
      await refresh();
    } catch (e) {
      notice = String(e);
    } finally {
      busy = false;
    }
  }

  async function saveAlias() {
    if (!selectedOutput || !aliasDraft.trim()) return;
    try {
      await api.setAlias(aliasDraft.trim(), selectedOutput.identity.connector);
      aliasDraft = "";
      await refresh();
    } catch (e) {
      notice = String(e);
    }
  }

  function select(o: Output) {
    selected = o.identity.connector;
    aliasDraft = o.alias ?? "";
  }
</script>

<h1>Displays</h1>

{#if firstRun}
  <div class="hint">
    <strong>New here?</strong> Click a display to name it ("TV", "left"), set
    things up how you like, then save your first profile on the
    <a href="/profiles">Profiles</a> page. The daemon keeps it asserted across
    reboots and hotplugs.
  </div>
{/if}

{#if notice}
  <div class="notice">{notice}</div>
{/if}

<div class="columns">
  <div class="canvas-col">
    <svg
      viewBox="0 0 {CANVAS_W} {CANVAS_H}"
      class="canvas"
      role="list"
      aria-label="Display layout"
    >
      {#each canvasRects as rect (rect.output.identity.connector)}
        <g
          role="button"
          aria-label={outputLabel(rect.output)}
          class="monitor"
          class:selected={selected === rect.output.identity.connector}
          onclick={() => select(rect.output)}
          onkeydown={(e) => e.key === "Enter" && select(rect.output)}
          tabindex="0"
        >
          <rect x={rect.x} y={rect.y} width={rect.w} height={rect.h} rx="6" />
          <text x={rect.x + rect.w / 2} y={rect.y + rect.h / 2 - 8}>
            {outputLabel(rect.output)}
          </text>
          <text class="sub" x={rect.x + rect.w / 2} y={rect.y + rect.h / 2 + 12}>
            {modeLabel(rect.output.mode)}{rect.output.primary
              ? " · primary"
              : ""}
          </text>
        </g>
      {/each}
      {#if enabled.length === 0}
        <text class="empty" x={CANVAS_W / 2} y={CANVAS_H / 2}>
          no active displays
        </text>
      {/if}
    </svg>

    {#if disabled.length > 0}
      <div class="shelf">
        <span class="shelf-label">off:</span>
        {#each disabled as o (o.identity.connector)}
          <button
            class="ghost"
            class:selected={selected === o.identity.connector}
            onclick={() => select(o)}
          >
            {outputLabel(o)}
          </button>
        {/each}
      </div>
    {/if}
  </div>

  <aside class="detail">
    {#if selectedOutput}
      <h2>{outputLabel(selectedOutput)}</h2>
      <dl>
        <dt>Connector</dt>
        <dd>{selectedOutput.identity.connector}</dd>
        <dt>Identity</dt>
        <dd>
          {#if selectedOutput.identity.edid}
            {selectedOutput.identity.edid.vendor}
            {selectedOutput.identity.edid.product}
            <span class="muted">SN {selectedOutput.identity.edid.serial}</span>
          {:else}
            <span class="muted">no EDID — matched by connector</span>
          {/if}
        </dd>
        <dt>State</dt>
        <dd>
          {selectedOutput.enabled ? "on" : "off"}
          {#if selectedOutput.primary}· primary{/if}
          {#if selectedOutput.builtin}· built-in panel{/if}
        </dd>
        <dt>Mode</dt>
        <dd>{modeLabel(selectedOutput.mode)}</dd>
        {#if selectedOutput.enabled}
          <dt>Position</dt>
          <dd>{selectedOutput.position[0]}, {selectedOutput.position[1]}</dd>
        {/if}
      </dl>

      <div class="actions">
        <button
          class="primary"
          disabled={busy}
          onclick={() => selectedOutput && toggle(selectedOutput)}
        >
          {selectedOutput.enabled ? "Turn off" : "Turn on"}
        </button>
      </div>

      <div class="alias">
        <label for="alias">Alias</label>
        <input
          id="alias"
          type="text"
          placeholder="TV, left, …"
          bind:value={aliasDraft}
        />
        <button onclick={saveAlias}>Save</button>
      </div>
    {:else}
      <p class="muted">Select a display to see details and controls.</p>
    {/if}
  </aside>
</div>

<style>
  .hint,
  .notice {
    padding: 0.6rem 0.9rem;
    border-radius: 8px;
    margin-bottom: 1rem;
    border: 1px solid var(--border);
    background: var(--surface);
  }
  .notice {
    border-color: color-mix(in srgb, var(--danger) 40%, transparent);
  }
  .columns {
    display: flex;
    gap: 1.25rem;
    align-items: flex-start;
  }
  .canvas-col {
    flex: 1;
    min-width: 0;
  }
  .canvas {
    width: 100%;
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: 10px;
  }
  .monitor rect {
    fill: color-mix(in srgb, var(--accent) 18%, var(--surface));
    stroke: var(--accent);
    stroke-width: 1.5;
    cursor: pointer;
  }
  .monitor.selected rect {
    stroke-width: 3;
  }
  .monitor text {
    text-anchor: middle;
    fill: var(--text);
    font-size: 15px;
    font-weight: 600;
    pointer-events: none;
  }
  .monitor text.sub {
    font-size: 11px;
    font-weight: 400;
    fill: var(--muted);
  }
  text.empty {
    text-anchor: middle;
    fill: var(--muted);
  }
  .shelf {
    margin-top: 0.75rem;
    display: flex;
    gap: 0.5rem;
    align-items: center;
    flex-wrap: wrap;
  }
  .shelf-label {
    color: var(--muted);
    font-size: 0.85rem;
  }
  .ghost {
    opacity: 0.65;
    border-style: dashed;
  }
  .ghost.selected {
    border-color: var(--accent);
    opacity: 1;
  }
  .detail {
    width: 280px;
    flex-shrink: 0;
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: 10px;
    padding: 1rem;
  }
  .detail h2 {
    margin-top: 0;
    font-size: 1.1rem;
  }
  dl {
    display: grid;
    grid-template-columns: auto 1fr;
    gap: 0.3rem 0.8rem;
    font-size: 0.9rem;
  }
  dt {
    color: var(--muted);
  }
  dd {
    margin: 0;
  }
  .muted {
    color: var(--muted);
  }
  .actions {
    margin: 0.9rem 0;
  }
  .alias {
    display: flex;
    gap: 0.4rem;
    align-items: center;
  }
  .alias input {
    flex: 1;
    min-width: 0;
  }
  .alias label {
    color: var(--muted);
    font-size: 0.85rem;
  }
</style>
