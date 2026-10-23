import {
  isCodexApiKeyAccount,
  isCodexChatgptPlanAccount,
  isCodexPendingOAuthAccount,
  isCodexWebSessionAccount,
  type CodexAccount,
} from '../types/codex';

const identityWords = (value?: string | null) => (value ?? '').trim()
  .replace(/[A-Z]/g, (letter) => letter.toLowerCase()).split(/[^a-z0-9]+/);

/** Local prefilter only; remote membership, management rights and seats are authoritative. */
export function hasPremiumSeatTeamIdentity(planType?: string | null, accountStructure?: string | null): boolean {
  const structure = identityWords(accountStructure);
  if (structure.some((word) => word === 'personal' || word === 'individual') || accountStructure?.includes('个人')) {
    return false;
  }
  if (structure.some((word) => ['team', 'workspace', 'business', 'enterprise', 'organization', 'edu'].includes(word))) {
    return true;
  }
  return identityWords(planType).some((word) => ['team', 'business', 'enterprise', 'edu'].includes(word));
}

export function isPremiumSeatTeamAccount(account: CodexAccount): boolean {
  return !isCodexApiKeyAccount(account)
    && !account.agent_identity
    && !isCodexChatgptPlanAccount(account)
    && !isCodexWebSessionAccount(account)
    && !isCodexPendingOAuthAccount(account)
    && Boolean(account.account_id?.trim() && account.user_id?.trim())
    && hasPremiumSeatTeamIdentity(account.plan_type, account.account_structure);
}
