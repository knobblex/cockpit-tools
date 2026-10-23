import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';
import vm from 'node:vm';
import ts from 'typescript';
import { isCodexIdTokenReauthReason, isCodexReauthRequiredError } from '../utils/codexSwitchAuthFailure.ts';

const source = ts.createSourceFile('controller.tsx', readFileSync(new URL('./useCodexAccountsLocalAccessController.tsx', import.meta.url), 'utf8'), ts.ScriptTarget.Latest, true);
let callback = '';
ts.forEachChild(source, function visit(node) {
  if (ts.isVariableDeclaration(node) && node.name.getText(source) === 'shouldOfferReauthorizeAction' && node.initializer && ts.isCallExpression(node.initializer)) {
    callback = node.initializer.arguments[0].getText(source);
  }
  ts.forEachChild(node, visit);
});
assert.ok(callback, 'quota reauthorization callback exists');
const context = vm.createContext({ isCodexIdTokenReauthReason, isCodexReauthRequiredError });
vm.runInContext(ts.transpileModule(`globalThis.offer = ${callback}`, { compilerOptions: { target: ts.ScriptTarget.ES2022 } }).outputText, context);

test('quota errors offer reauthorization only after a terminal credential failure', () => {
  for (const [errorCode, rawMessage, expected] of [
    ['token_expired', '刷新失败：API 返回错误 401 Unauthorized [body:{"message":"Your authentication token has expired. Please try refreshing it."}]', false],
    ['', 'API 返回错误 401 Unauthorized', false],
    ['invalid_token', 'API 返回错误 401 Unauthorized', false],
    ['refresh_token_reused', 'Token 刷新失败: status=401 Unauthorized', false],
    ['unsupported_country_region_territory', 'Token 刷新失败: status=401 Unauthorized', false],
    ['refresh_token_expired', 'Token 刷新失败: status=401 Unauthorized', true],
    ['refresh_token_invalidated', 'request rejected', true],
    ['token_revoked', 'API 返回错误 401 Unauthorized', true],
    ['invalid_grant', 'request rejected', true],
    ['', '缺少 refresh_token', true],
    ['token_expired', 'CODEX_QUOTA_AUTH_RECOVERY_EXHAUSTED: API 返回错误 401 Unauthorized', true],
  ] as const) {
    assert.equal(context.offer({ statusCode: '401', errorCode, rawMessage }), expected, rawMessage);
  }
});
