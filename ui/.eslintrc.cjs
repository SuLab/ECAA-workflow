// Minimal ESLint config focused on catching the bug classes that have
// bitten this codebase: missing useEffect deps, hooks-rule violations,
// stale closure perils. Style rules are not enforced — formatting is
// up to authorial discretion (no Prettier in CI either).
//
// Run via: make lint-ui  (or: cd ui && npx eslint src --ext .ts,.tsx)
// CI gate: wired into ui-typecheck job in .github/workflows/rust.yml
//          and joined to `make product` / `make all`.

/** @type {import('eslint').Linter.Config} */
module.exports = {
  root: true,
  parser: '@typescript-eslint/parser',
  parserOptions: {
    ecmaVersion: 2022,
    sourceType: 'module',
    ecmaFeatures: { jsx: true },
  },
  plugins: ['@typescript-eslint', 'react-hooks'],
  extends: [
    'eslint:recommended',
    'plugin:@typescript-eslint/recommended',
  ],
  rules: {
    'react-hooks/rules-of-hooks': 'error',
    'react-hooks/exhaustive-deps': 'warn',
    // Allow underscore-prefixed unused vars — used for exhaustiveness
    // sentinels (`const _exhaustive: never = ...`).
    '@typescript-eslint/no-unused-vars': [
      'warn',
      { argsIgnorePattern: '^_', varsIgnorePattern: '^_' },
    ],
    // Loose on `any` since the wire types use `unknown` widely and
    // the codebase has only one `as any` (in useSseChatEvents for the
    // legacy remote field shape, intentionally tagged).
    '@typescript-eslint/no-explicit-any': 'off',
  },
  ignorePatterns: ['dist/', 'node_modules/', 'src/types/'],
  env: {
    browser: true,
    es2022: true,
  },
  settings: {
    react: { version: '18' },
  },
}
