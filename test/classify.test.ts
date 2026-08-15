import test from 'node:test'
import assert from 'node:assert/strict'
import { classifyPath, globToRegExp } from '../src/classify.ts'
import { toRepoRelative } from '../src/extract.ts'

// The classifier that ships in the policy bundle (eng finding 9). This is the
// M1 bundle exactly as coding-bundle.service.ts builds it, in its order, because
// first match wins and the order is the contract.
const CLASSIFIER: Record<string, string[]> = {
  tests: ['**/*.test.*', '**/*.spec.*', '**/tests/**', '**/__tests__/**', 'test/**'],
  auth: ['**/auth/**', '**/*auth*', '**/session*', '**/*jwt*', '**/*passkey*'],
  security: ['**/crypto/**', '**/*secret*', '**/*credential*', '**/security/**'],
  payments: ['**/billing/**', '**/payments/**', '**/*stripe*', '**/*invoice*'],
  infra: ['**/Dockerfile*', '**/docker-compose*.yml', '**/*.tf', '.github/workflows/**', 'deploy/**', 'k8s/**'],
  migrations: ['**/migrations/**'],
  config: ['**/*.env*', '**/*.config.*', '**/*.yaml', '**/*.yml'],
  docs: ['**/*.md', 'docs/**'],
  frontend: ['**/*.tsx', '**/components/**', '**/styles/**'],
  backend: ['**/*.ts', '**/*.py', '**/*.go', '**/*.rs', 'src/**'],
}

test('paths classify to the first matching class, in bundle order', () => {
  const cases: [string, string | null][] = [
    ['src/integrations/coding/coding.spec.ts', 'tests'],
    ['test/e2e/login.ts', 'tests'],
    ['src/auth/jwt.strategy.ts', 'auth'],
    ['src/billing/invoice.ts', 'payments'],
    ['deploy/k8s.yaml', 'infra'],
    ['src/database/migrations/1721200000000-Coding.ts', 'migrations'],
    ['README.md', 'docs'],
    ['docs/architecture.md', 'docs'],
    ['src/components/button.tsx', 'frontend'],
    ['src/main.ts', 'backend'],
    ['LICENSE', null],
  ]
  for (const [path, expected] of cases) {
    assert.equal(classifyPath(CLASSIFIER, path), expected, path)
  }
})

test('a spec file is tests even though it is also a .ts file', () => {
  // The ordering assertion that matters: `backend` would swallow every source
  // file if the classifier were iterated in any order but insertion order.
  assert.equal(classifyPath(CLASSIFIER, 'src/auth/auth.service.spec.ts'), 'tests')
})

test('a leading **/ also matches at the repository root', () => {
  assert.ok(globToRegExp('**/*.md').test('README.md'))
  assert.ok(globToRegExp('**/*.md').test('docs/deep/file.md'))
  assert.equal(globToRegExp('**/*.md').test('README.txt'), false)
})

test('a single star does not cross a directory boundary', () => {
  assert.ok(globToRegExp('src/*.ts').test('src/main.ts'))
  assert.equal(globToRegExp('src/*.ts').test('src/deep/main.ts'), false)
})

test('a path outside the repository classifies as nothing', () => {
  // The alternative is confident nonsense: an absolute path from somewhere else
  // on the disk matched against repo-relative globs.
  assert.equal(toRepoRelative('/Users/someone/notes/auth-notes.md', '/repo'), '')
  assert.equal(classifyPath(CLASSIFIER, toRepoRelative('/Users/someone/notes/auth-notes.md', '/repo')), null)
  assert.equal(toRepoRelative('/repo/src/auth/x.ts', '/repo'), 'src/auth/x.ts')
})

test('backslash paths classify the same as forward slash paths', () => {
  assert.equal(classifyPath(CLASSIFIER, 'src\\auth\\jwt.ts'), 'auth')
})
