import type {
  CapturedEmail,
  EmailListResponse,
  Health,
  KeyListResponse,
  ResetReport,
  Stats,
  WebhookListResponse,
} from "@/types/admin"

/**
 * The admin API client.
 *
 * Same origin in both modes: the binary serves the console from the API port, and in
 * development Vite proxies `/_admin` there. So no base URL and no CORS.
 */
const BASE = "/_admin"

/**
 * An admin API failure, carrying the Resend error `name` so callers can branch on it.
 *
 * The name is the contract, not the message — the same rule the server's own tests
 * follow.
 */
export class AdminApiError extends Error {
  readonly code: string
  readonly status: number

  constructor(status: number, code: string, message: string) {
    super(message)
    this.name = "AdminApiError"
    this.code = code
    this.status = status
  }
}

async function request<T>(path: string, init?: RequestInit): Promise<T> {
  const response = await fetch(`${BASE}${path}`, init)

  if (!response.ok) {
    throw await toError(response)
  }

  if (response.status === 204) {
    return undefined as T
  }

  return (await response.json()) as T
}

/**
 * Turn a failed response into an error worth showing someone.
 *
 * mock-resend answers with Resend's own `{ statusCode, message, name }`, but a crashed
 * or proxied request can return anything, so a body that is not that shape falls back to
 * the status rather than throwing a parse error over the real problem.
 */
async function toError(response: Response): Promise<AdminApiError> {
  try {
    const body = (await response.json()) as { name?: string; message?: string }
    if (body.name) {
      return new AdminApiError(response.status, body.name, body.message ?? body.name)
    }
  } catch {
    // Not JSON. The status is all we have.
  }

  return new AdminApiError(response.status, "unknown", `The admin API answered ${response.status}.`)
}

const json = { "content-type": "application/json" }

export const admin = {
  health: () => request<Health>("/health"),

  keys: () => request<KeyListResponse>("/keys"),

  /** Captured mail, across every key or under one of them, newest first. */
  mailbox: (apiKey?: string, limit = 200) => {
    const params = new URLSearchParams({ limit: String(limit) })
    if (apiKey) {
      params.set("api_key", apiKey)
    }
    return request<EmailListResponse>(`/mailbox?${params.toString()}`)
  },

  /** One message, found without being told which key owns it. */
  email: (id: string) => request<CapturedEmail>(`/mailbox/${encodeURIComponent(id)}`),

  stats: (apiKey: string) => request<Stats>(`/stats?api_key=${encodeURIComponent(apiKey)}`),

  webhooks: (apiKey: string) =>
    request<WebhookListResponse>(`/webhooks?api_key=${encodeURIComponent(apiKey)}`),

  reset: (apiKey?: string) =>
    request<ResetReport>("/reset", {
      method: "POST",
      headers: json,
      body: JSON.stringify(apiKey ? { api_key: apiKey } : {}),
    }),

  advanceEvent: (id: string, apiKey: string, event: string) =>
    request<unknown>(`/emails/${encodeURIComponent(id)}/events`, {
      method: "POST",
      headers: json,
      body: JSON.stringify({ api_key: apiKey, event }),
    }),

  /**
   * Where the raw `.eml` is served from.
   *
   * A URL rather than a fetch so the Raw tab can stream it and a developer can open it
   * in a new tab, which is often what they actually want.
   */
  rawUrl: (id: string) => `${BASE}/mailbox/${encodeURIComponent(id)}/raw`,

  /** The same bytes as text, for the Raw tab. */
  raw: async (id: string): Promise<string> => {
    const response = await fetch(admin.rawUrl(id))
    if (!response.ok) {
      throw await toError(response)
    }
    return response.text()
  },

  /**
   * Where one attachment's bytes are served from, with the type it declared.
   *
   * Addressed by position, not by filename: a filename is a caller-supplied string that
   * can repeat, be empty, or contain anything at all.
   */
  attachmentUrl: (id: string, index: number, download = false) => {
    const suffix = download ? "?download=1" : ""
    return `${BASE}/mailbox/${encodeURIComponent(id)}/attachments/${index}${suffix}`
  },
}
