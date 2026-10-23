import { invoke } from "@tauri-apps/api/core";

export interface PremiumSeatConfig {
  enabled: boolean;
  workspaceId: string;
  adminAccountId: string;
  apiKeyId: string;
  useApiKeyPool?: boolean;
  /** Legacy authorization boundary; new settings use the key's effective pool. */
  memberAccountIds: string[];
  probeModel: string;
}
export interface PremiumSeatOperation {
  id: string;
  workspaceId: string;
  sourceAccountId: string;
  targetAccountId: string;
  sourceUserId: string;
  targetUserId: string;
  beforeCounts: Record<string, number>;
  createdAt: number;
  submittedAt: number | null;
  exhaustionTriggered: boolean | null;
  probeStarted: boolean;
  verifiedAt: number | null;
  resolvedWithoutSwap: boolean;
}
export interface PremiumSeatState {
  version: number;
  revision: number;
  config: PremiumSeatConfig | null;
  paused: boolean;
  phase: "idle" | "ready" | "draining" | "submitted" | "verifying" | "needs_attention";
  holderAccountId: string | null;
  holderUserId: string | null;
  validatedAt: number | null;
  operation: PremiumSeatOperation | null;
  history: PremiumSeatOperation[];
  exhaustedUntil: Record<string, number>;
  message: string | null;
  nextCheckAt: number | null;
}
export const getPremiumSeat = () => invoke<PremiumSeatState>("codex_premium_seat_get");
export const checkPremiumSeat = (config: PremiumSeatConfig) => invoke<PremiumSeatState>("codex_premium_seat_check", { config });
export const configurePremiumSeat = (config: PremiumSeatConfig) => invoke<PremiumSeatState>("codex_premium_seat_configure", { config });
export const controlPremiumSeat = (action: "pause" | "resume" | "resolve_unapplied" | "next") => invoke<PremiumSeatState>("codex_premium_seat_control", { action });
