import { readCredentials, writeCredentials } from './store.ts'
import type { Credentials } from './store.ts'
import type {
  AgentId,
  CodingEventBatch,
  DeviceCodeGrant,
  SessionStartRequest,
  SessionStartResponse,
  TokenResponse,
} from './types.ts'

export const CLIENT_VERSION = '0.1.0-spike.1'
export const DEFAULT_CLIENT_ID = 'flueny-claude-code'
// Production. This is only a fallback: `login` prefers --api-url, then
// FLUENY_API_URL, and every hook afterwards reads the apiUrl stored in the
// credential, never this constant. It matters for exactly one case, a fresh
// machine running `flueny login` with no flag, where a localhost default
// fails against the developer's own laptop and says nothing about why.
export const DEFAULT_API_URL = 'https://api.flueny.dev'

// Every network call a hook makes is on the developer's critical path, so all of
// them are bounded. A Flueny outage must cost a couple of seconds once, never a
// hung editor: that is the client-side half of CEO decision 6A, which promises a
// developer never sees Flueny's infrastructure failing.
const HOOK_TIMEOUT_MS = 2500

// The backend is NestJS, so a POST with no explicit @HttpCode answers 201, not
// 200. `/session/start` is one of those and `/oauth/device` is another, while
// `/events` is an explicit 202 and `/oauth/token` an explicit 200. A client that
// checked for 200 would read every successful handshake as a failure and go
// inert forever, which is exactly what happened the first time this ran against a
// live backend. Nothing here compares a status to a literal.
export function isOk(status: number): boolean {
  return status >= 200 && status < 300
}

export interface HttpResult<T> {
  status: number
  body: T | null
  text: string
}

export async function request<T>(
  base: string,
  method: string,
  path: string,
  opts: { token?: string; body?: unknown; timeoutMs?: number } = {},
): Promise<HttpResult<T>> {
  const controller = new AbortController()
  const timer = setTimeout(() => controller.abort(), opts.timeoutMs ?? HOOK_TIMEOUT_MS)
  try {
    const res = await fetch(`${base}${path}`, {
      method,
      headers: {
        'content-type': 'application/json',
        ...(opts.token ? { authorization: `Bearer ${opts.token}` } : {}),
      },
      body: opts.body === undefined ? undefined : JSON.stringify(opts.body),
      signal: controller.signal,
    })
    const text = await res.text()
    let body: T | null = null
    try {
      body = text ? (JSON.parse(text) as T) : null
    } catch {
      body = null
    }
    return { status: res.status, body, text }
  } catch (err) {
    return { status: 0, body: null, text: err instanceof Error ? err.message : String(err) }
  } finally {
    clearTimeout(timer)
  }
}

// ---- OAuth device authorization grant (RFC 8628, CEO decision 15A) ----

export function startDevice(
  base: string,
  clientId: string,
  agent: AgentId,
  deviceLabel: string,
): Promise<HttpResult<DeviceCodeGrant>> {
  return request<DeviceCodeGrant>(base, 'POST', '/integrations/coding/oauth/device', {
    body: { clientId, agent, deviceLabel },
    timeoutMs: 10_000,
  })
}

export function exchangeDeviceCode(
  base: string,
  clientId: string,
  deviceCode: string,
): Promise<HttpResult<TokenResponse & { error?: string }>> {
  return request(base, 'POST', '/integrations/coding/oauth/token', {
    body: {
      grant_type: 'urn:ietf:params:oauth:grant-type:device_code',
      client_id: clientId,
      device_code: deviceCode,
    },
    timeoutMs: 10_000,
  })
}

export function exchangeRefreshToken(
  base: string,
  clientId: string,
  refreshToken: string,
): Promise<HttpResult<TokenResponse & { error?: string }>> {
  return request(base, 'POST', '/integrations/coding/oauth/token', {
    body: { grant_type: 'refresh_token', client_id: clientId, refresh_token: refreshToken },
    timeoutMs: 10_000,
  })
}

// Hook tokens are short lived by design (eng finding 5). Refreshing a minute
// early costs one round trip; refreshing on the 401 costs a round trip on a hook
// that is already holding up a tool call.
const REFRESH_MARGIN_MS = 60_000

export async function currentToken(agent: AgentId): Promise<Credentials | null> {
  const creds = readCredentials(agent)
  if (!creds) return null
  if (creds.expiresAt - REFRESH_MARGIN_MS > Date.now()) return creds
  return refresh(creds, agent)
}

export async function refresh(creds: Credentials, agent: AgentId): Promise<Credentials | null> {
  const res = await exchangeRefreshToken(creds.apiUrl, creds.clientId, creds.refreshToken)
  if (!isOk(res.status) || !res.body?.access_token) return null
  const next: Credentials = {
    ...creds,
    agent,
    accessToken: res.body.access_token,
    refreshToken: res.body.refresh_token ?? creds.refreshToken,
    expiresAt: Date.now() + (res.body.expires_in ?? 3600) * 1000,
  }
  writeCredentials(next, agent)
  return next
}

// ---- the two client-facing endpoints ----

export function sessionStart(
  base: string,
  token: string,
  req: SessionStartRequest,
): Promise<HttpResult<SessionStartResponse>> {
  return request<SessionStartResponse>(base, 'POST', '/integrations/coding/session/start', {
    token,
    body: req,
  })
}

// Always 202, including on malformed input and on load shedding, so the status
// says nothing about whether the events were kept. Nothing in this client may
// branch on it as if it did (CEO decision 6A). The one exception is 401, which
// is the client's own credential expiring and is the caller's cue to refresh.
export function postEvents(
  base: string,
  token: string,
  batch: CodingEventBatch,
): Promise<HttpResult<unknown>> {
  return request(base, 'POST', '/integrations/coding/events', { token, body: batch })
}
