// Typed wrappers over the Tauri commands (which proxy the daemon IPC).

import { invoke } from "@tauri-apps/api/core";

export interface EdidId {
  vendor: string;
  product: string;
  serial: string;
}

export interface OutputIdentity {
  edid: EdidId | null;
  connector: string;
}

export interface Mode {
  width: number;
  height: number;
  refresh_mhz: number;
}

export interface Output {
  identity: OutputIdentity;
  alias: string | null;
  display_name: string;
  builtin: boolean;
  enabled: boolean;
  mode: Mode | null;
  preferred_mode: Mode | null;
  modes: Mode[];
  position: [number, number];
  scale: number;
  transform: string;
  primary: boolean;
}

export interface Topology {
  serial: string;
  outputs: Output[];
}

export type SetOutcome =
  | { outcome: "applied"; warnings: unknown[] }
  | { outcome: "already_in_state" }
  | {
      outcome: "pending";
      job_id: number;
      deadline_secs: number;
      risk: string;
      warnings: unknown[];
    }
  | { outcome: "guard_refused"; reason: string };

export interface ProfileInfo {
  name: string;
  hotkey: string | null;
  active: boolean;
  drifted: boolean;
}

export interface ProfilesInfo {
  profiles: ProfileInfo[];
  active: string | null;
  suspended: boolean;
}

export interface PendingInfo {
  job_id: number;
  deadline_secs: number;
  risk: string;
}

export interface StatusInfo {
  version: string;
  backend: string;
  active_profile: string | null;
  suspended: boolean;
  drifted: boolean;
  pending: PendingInfo | null;
  outputs_total: number;
  outputs_enabled: number;
}

export const api = {
  outputs: () => invoke<Topology>("outputs"),
  setOutput: (
    target: string,
    mode: "on" | "off" | "toggle",
    force = false,
    revertSecs: number | null = null,
  ) => invoke<SetOutcome>("set_output", { target, mode, force, revertSecs }),
  confirmPending: () => invoke<boolean>("confirm_pending"),
  revertPending: () => invoke<boolean>("revert_pending"),
  profiles: () => invoke<ProfilesInfo>("profiles"),
  profileApply: (name: string) => invoke<SetOutcome>("profile_apply", { name }),
  profileSave: (name: string) => invoke<void>("profile_save", { name }),
  profileDelete: (name: string) => invoke<boolean>("profile_delete", { name }),
  setAlias: (alias: string, target: string) =>
    invoke<void>("set_alias", { alias, target }),
  status: () => invoke<StatusInfo>("status"),
  setCloseToTray: (enabled: boolean) =>
    invoke<void>("set_close_to_tray", { enabled }),
  daemonStart: () => invoke<void>("daemon_start"),
};

export function modeLabel(mode: Mode | null): string {
  if (!mode) return "—";
  return `${mode.width}×${mode.height} @ ${(mode.refresh_mhz / 1000).toFixed(0)} Hz`;
}

export function outputLabel(output: Output): string {
  return output.alias ?? output.display_name ?? output.identity.connector;
}
