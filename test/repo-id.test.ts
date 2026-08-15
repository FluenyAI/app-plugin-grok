import test from 'node:test'
import assert from 'node:assert/strict'
import { normalizeRemote, repoIdFor } from '../src/repo-id.ts'

// Normalization is a cross-repo contract. If this client and the backend disagree
// by one character, every event from a correctly allowlisted repository is
// dropped and CEO decision 6A guarantees nobody sees an error: the developer's
// dashboard is simply blank forever.
//
// The hash below is the one app-backend pins in coding-repo-id.spec.ts. It is
// written out rather than imported, because a shared import would only prove the
// two files agree with themselves.
const PINNED = 'sha256:97992c958c94ae63d8a4a35d948f6d5f1a49d93a158497f9bf401872ff45d2ae'

test('every spelling of one remote produces the pinned id', () => {
  const spellings = [
    'git@github.com:FluenyAI/app-backend.git',
    'https://github.com/fluenyai/app-backend',
    'ssh://git@github.com:22/FluenyAI/app-backend.git',
    'https://user:token@github.com/fluenyai/app-backend.git/',
    'git+https://github.com/FluenyAI/app-backend.git',
    '  https://github.com/FluenyAI/app-backend/  ',
  ]
  for (const remote of spellings) {
    assert.equal(normalizeRemote(remote), 'github.com/fluenyai/app-backend', remote)
    assert.equal(repoIdFor(remote), PINNED, remote)
  }
})

test('a remote that normalizes to nothing produces no id', () => {
  // An empty id would otherwise match every event that carries no repository,
  // which is the one way fail-closed turns into fail-open.
  assert.equal(repoIdFor(''), '')
  assert.equal(repoIdFor('   '), '')
})

test('different repositories do not collide', () => {
  assert.notEqual(repoIdFor('git@github.com:FluenyAI/app-frontend.git'), PINNED)
})
