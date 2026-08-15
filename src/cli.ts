#!/usr/bin/env node
import { hostname } from 'node:os'
import {
  CLIENT_VERSION,
  DEFAULT_API_URL,
  DEFAULT_CLIENT_ID,
  currentToken,
  exchangeDeviceCode,
  isOk,
  sessionStart,
  startDevice,
} from './api.ts'
import { setupConnected, weeklySummary, wrap } from './copy.ts'
import { onPostToolUse, onSessionEnd, onSessionStart, onStop } from './hooks.ts'
import { entriesFor, receiptFor } from './receipt.ts'
import { detectAgent } from './session.ts'
import type { AgentId } from './types.ts'
import {
  clearCredentials,
  configDir,
  listCredentialAgents,
  readBundle,
  readCounters,
  readCredentials,
  readLedger,
  readQueue,
  readReceiptDay,
  today,
  writeCredentials,
  writeReceiptDay,
} from './store.ts'
import { settingsFragment } from './settings.ts'
import { readsLocallyDeclaration } from './reads.ts'

// `flueny`. Six commands, one of which (`hook`) is what Claude Code calls and the
// other five of which a developer calls.
//
// Nothing here prints colour, an emoji or a box character, and nothing exceeds 80
// columns. That is the terminal voice from the design plan, and it is also just
// what survives a CI log and a piped stdout.

async function main(argv: string[]): Promise<number> {
  const [command, ...rest] = argv
  switch (command) {
    case 'login':
      return login(rest)
    case 'logout':
      return logout()
    case 'status':
      return status()
    case 'dry-run':
      return dryRun(rest)
    case 'hook':
      return hook(rest)
    case 'install':
      return install(rest)
    default:
      return usage()
  }
}

function agentLabel(agent: AgentId): string {
  if (agent === 'grok-build') return 'Grok'
  if (agent === 'claude-code') return 'Claude Code'
  return agent
}

function usage(): number {
  say(
    [
      'flueny, the Flueny client for Claude Code and Grok (dogfood spike).',
      '',
      '  flueny login [--api-url URL] [--agent AGENT] [--label NAME]',
      '  flueny status                                 what this client is doing',
      '  flueny dry-run --today                        what was sent today, in full',
      '  flueny install [--print]                      the hook settings to merge by hand',
      '  flueny logout                                 forget the credential',
      '  flueny hook <event>                           called by the agent, reads stdin',
    ].join('\n'),
  )
  return 0
}

// ---- login: OAuth device authorization grant (CEO decision 15A) ----

async function login(argv: string[]): Promise<number> {
  const apiUrl = (flag(argv, '--api-url') ?? process.env.FLUENY_API_URL ?? DEFAULT_API_URL).replace(/\/+$/, '')
  const clientId = flag(argv, '--client-id') ?? DEFAULT_CLIENT_ID
  const label = flag(argv, '--label') ?? hostname()
  const agent = detectAgent(flag(argv, '--agent'))

  const grant = await startDevice(apiUrl, clientId, agent, label)
  if (!isOk(grant.status) || !grant.body) {
    say(`Flueny could not start sign-in against ${apiUrl} (${grant.status}).`)
    return 1
  }

  say(
    [
      `Open ${grant.body.verification_uri} and enter this code:`,
      '',
      `    ${grant.body.user_code}`,
      '',
      `Or open ${grant.body.verification_uri_complete}`,
      'Waiting for approval. Ctrl-C to stop.',
    ].join('\n'),
  )

  const deadline = Date.now() + grant.body.expires_in * 1000
  const intervalMs = Math.max(1, grant.body.interval) * 1000
  for (;;) {
    if (Date.now() > deadline) {
      say('The code expired before it was approved. Run flueny login again.')
      return 1
    }
    await sleep(intervalMs)
    const token = await exchangeDeviceCode(apiUrl, clientId, grant.body.device_code)
    if (isOk(token.status) && token.body?.access_token) {
      writeCredentials(
        {
          apiUrl,
          appUrl: originOf(grant.body.verification_uri),
          clientId,
          accessToken: token.body.access_token,
          refreshToken: token.body.refresh_token,
          expiresAt: Date.now() + (token.body.expires_in ?? 3600) * 1000,
          agent,
        },
        agent,
      )
      return afterLogin(apiUrl, token.body.access_token, grant.body.verification_uri, agent)
    }
    // RFC 8628: authorization_pending means keep polling, slow_down means back
    // off. Anything else is terminal and repeating it just wastes the developer's
    // time on a code that will never be approved.
    const error = errorCode(token.text)
    if (error === 'authorization_pending') continue
    if (error === 'slow_down') {
      await sleep(intervalMs)
      continue
    }
    say(`Sign-in failed: ${error ?? token.status}. Run flueny login again.`)
    return 1
  }
}

// The handshake runs once here so the setup line can state the real dry-run
// window rather than the default from the design plan. "Never print a number the
// product cannot substantiate" is a rule about this exact sentence.
async function afterLogin(
  apiUrl: string,
  token: string,
  verificationUri: string,
  agent: AgentId,
): Promise<number> {
  const res = await sessionStart(apiUrl, token, {
    agent,
    sessionId: `setup-${Date.now()}`,
    clientVersion: CLIENT_VERSION,
    bundleEtag: readBundle()?.etag ?? null,
    readsLocally: readsLocallyDeclaration(),
  })
  const appUrl = originOf(verificationUri)
  const dryRun = res.body?.dryRun ?? false
  say('')
  say(setupConnected({ dryRun, dryRunDays: daysUntil(res.body?.dryRunEndsAt ?? null), appUrl }))
  if (res.body?.killSwitch) {
    say('')
    say('Flueny is turned off for your organisation. This client will send nothing.')
  }
  say('')
  say(`Next: flueny install, then restart ${agent === 'grok-build' ? 'Grok' : 'Claude Code'}.`)
  return 0
}

function logout(): number {
  const agent = detectAgent()
  clearCredentials(agent)
  const remaining = listCredentialAgents()
  say(`Disconnected ${agentLabel(agent)} on this machine.`)
  if (remaining.length > 0) {
    say(`Still connected: ${remaining.map(agentLabel).join(', ')}.`)
  } else {
    say('Flueny will send nothing until you run flueny login again.')
  }
  return 0
}

// ---- status ----

async function status(): Promise<number> {
  const agent = detectAgent()
  const creds = readCredentials(agent)
  const others = listCredentialAgents().filter((a) => a !== agent)
  if (!creds) {
    if (others.length > 0) {
      say(`${agentLabel(agent)} is not connected on this machine.`)
      say(`Still connected: ${others.map(agentLabel).join(', ')}.`)
      say(`Run flueny login --agent ${agent} to connect this host.`)
    } else {
      say('Flueny is not connected on this machine. Run flueny login.')
    }
    return 0
  }
  const bundle = readBundle()
  const queued = readQueue()
  const counters = readCounters(today())

  const lines = [
    `API              ${creds.apiUrl}`,
    `Agent            ${agentLabel(agent)}`,
    `Also connected   ${others.length > 0 ? others.map(agentLabel).join(', ') : 'none'}`,
    `Credential       ${creds.expiresAt > Date.now() ? 'valid' : 'expired, refreshes on next hook'}`,
    `Config           ${configDir()}`,
    `Policy bundle    ${bundle ? `etag ${bundle.etag}, schema ${bundle.schemaVersion}` : 'not cached yet'}`,
    `Queued events    ${queued.length}`,
    `Observed today   ${counters.observed}`,
    `Sent today       ${counters.wouldSend}`,
    `Blocked today    ${counters.wouldBlock}`,
  ]
  say(lines.join('\n'))

  const refreshed = await currentToken(agent)
  if (!refreshed) {
    say('')
    say('The credential could not be refreshed. Run flueny login again.')
  }
  say('')
  say(receiptFor())
  const summary = weekly(creds.apiUrl, creds.appUrl)
  if (summary) {
    say('')
    say(summary)
  }
  return 0
}

// The weekly summary is rendered from the local ledger, so it can only ever
// state what this machine actually derived. `/coding/signal` stays authoritative
// (design decision 46); this is additive and says nothing the ledger cannot show.
function weekly(apiUrl: string, appUrl?: string): string | null {
  let decisions = 0
  let rejected = 0
  let touchedTests = 0
  for (let i = 0; i < 7; i++) {
    const day = today(new Date(Date.now() - i * 86_400_000))
    for (const entry of readLedger(day)) {
      if (!entry.summary.startsWith('Agent edit ')) continue
      decisions += 1
      if (entry.summary.startsWith('Agent edit rejected')) {
        rejected += 1
        if (entry.summary.includes('path class tests')) touchedTests += 1
      }
    }
  }
  if (decisions === 0) return null
  return weeklySummary({ rejected, decisions, touchedTests, appUrl: appUrl ?? originOf(apiUrl) })
}

// ---- dry-run ----

function dryRun(argv: string[]): number {
  if (!argv.includes('--today')) {
    say('Usage: flueny dry-run --today')
    return 1
  }
  const day = today()
  say(receiptFor(day))
  const entries = entriesFor(day)
  if (entries.length === 0) {
    say('')
    say('Nothing has been sent today.')
    return 0
  }
  say('')
  say(`Every row Flueny sent today, ${entries.length} in total:`)
  say('')
  for (const entry of entries) {
    say(`  ${entry.at.slice(11, 19)}  ${entry.summary}`)
    say(`            fields: ${entry.fieldsSent.join(', ')}`)
  }
  say('')
  say(wrap('These field names are the whole payload. No prompt, no code and no file contents are in it.').join('\n'))
  markReceiptShown(day)
  return 0
}

function markReceiptShown(day: string): void {
  if (readReceiptDay() !== day) writeReceiptDay(day)
}

// ---- hook: what Claude Code calls ----

async function hook(argv: string[]): Promise<number> {
  const event = argv[0] ?? ''
  const payload = await readStdinJson()
  try {
    switch (event) {
      case 'session-start':
        await onSessionStart(payload)
        break
      case 'post-tool-use':
        await onPostToolUse(payload)
        break
      case 'stop':
        await onStop(payload)
        break
      case 'session-end':
        await onSessionEnd(payload)
        break
      default:
        return 0
    }
  } catch {
    // A hook that fails is a hook that must fail silently. Claude Code shows a
    // non-zero exit to the developer, and CEO decision 6A is that a developer
    // never sees Flueny's plumbing. Nothing here is worth interrupting work for.
    return 0
  }
  return 0
}

// Hook payloads arrive on stdin as one JSON object. Nothing read here is written
// anywhere: extraction happens in src/extract.ts and the raw object is discarded
// with this process.
async function readStdinJson(): Promise<Record<string, unknown>> {
  if (process.stdin.isTTY) return {}
  const chunks: Buffer[] = []
  for await (const chunk of process.stdin) chunks.push(chunk as Buffer)
  try {
    const parsed: unknown = JSON.parse(Buffer.concat(chunks).toString('utf8'))
    return typeof parsed === 'object' && parsed !== null ? (parsed as Record<string, unknown>) : {}
  } catch {
    return {}
  }
}

// ---- install ----

function install(argv: string[]): number {
  const fragment = settingsFragment(process.argv[1] ?? 'flueny')
  say(JSON.stringify(fragment, null, 2))
  if (argv.includes('--print')) return 0
  say('')
  say(
    wrap(
      'Merge the hooks block above into ~/.claude/settings.json, or into ' +
        '.claude/settings.json in one repository, then restart Claude Code. Every ' +
        'hook is type command on purpose: extraction runs here, and an http hook ' +
        'would post raw tool payloads off this machine.',
    ).join('\n'),
  )
  return 0
}

// ---- helpers ----

function flag(argv: string[], name: string): string | null {
  const at = argv.indexOf(name)
  return at >= 0 ? (argv[at + 1] ?? null) : null
}

function errorCode(text: string): string | null {
  try {
    const body: unknown = JSON.parse(text)
    if (typeof body === 'object' && body !== null && 'error' in body) {
      const value = (body as { error: unknown }).error
      return typeof value === 'string' ? value : null
    }
  } catch {
    // Not a JSON body. Treated as terminal, which it is.
  }
  return null
}

export function daysUntil(iso: string | null): number {
  if (!iso) return 0
  const ms = Date.parse(iso) - Date.now()
  return ms <= 0 ? 0 : Math.ceil(ms / 86_400_000)
}

function originOf(url: string): string {
  try {
    return new URL(url).origin
  } catch {
    return url
  }
}

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms))
}

function say(text: string): void {
  process.stdout.write(`${text}\n`)
}

const code = await main(process.argv.slice(2))
process.exit(code)
