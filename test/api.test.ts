import test from 'node:test'
import assert from 'node:assert/strict'
import { describeApi, resolveApiTarget } from '../src/api-url.ts'

// Pointing a machine at the wrong Flueny sends one company's derived signal to
// another company's server, so this resolves names and URLs and refuses
// everything else rather than guessing a scheme or a hostname shape.

test('the two environments are reachable by name', () => {
  assert.equal(resolveApiTarget('staging'), 'https://api.flueny.dev')
  assert.equal(resolveApiTarget('production'), 'https://api.flueny.ai')
})

test('a name is matched however it was typed', () => {
  assert.equal(resolveApiTarget('  Production '), 'https://api.flueny.ai')
})

test('a full URL is accepted, for a local or self-hosted Flueny', () => {
  assert.equal(resolveApiTarget('http://localhost:3011'), 'http://localhost:3011')
  assert.equal(resolveApiTarget('https://flueny.acme.internal/'), 'https://flueny.acme.internal')
})

test('anything else is refused rather than guessed at', () => {
  // A bare hostname is the dangerous one: assuming https:// here would happily
  // point the client at a typo of the real host.
  assert.equal(resolveApiTarget('api.flueny.ai'), null)
  assert.equal(resolveApiTarget('prod'), null)
  assert.equal(resolveApiTarget(''), null)
  assert.equal(resolveApiTarget('ftp://api.flueny.ai'), null)
})

test('a known URL is described by name, an unknown one by itself', () => {
  assert.equal(describeApi('https://api.flueny.dev'), 'staging (https://api.flueny.dev)')
  assert.equal(describeApi('http://localhost:3011'), 'http://localhost:3011')
})
