/**
 * The shapes the admin API answers with.
 *
 * Hand-written rather than generated: the surface is small, and a generated client would
 * add a build step and a committed artifact to keep in sync for no gain at this size.
 * These mirror `crates/mock-resend-admin/src/wire.rs`.
 */

export type Health = {
  status: string
  version: string
  /** The engine's time, which may be ahead of the wall clock after `/_admin/clock`. */
  now: string
}

/** One API key, which is the namespace everything in mock-resend lives in. */
export type KeySummary = {
  api_key: string
  captured: number
  last_captured_at: string | null
}

export type KeyListResponse = {
  count: number
  data: KeySummary[]
}

/** What a send asked to be attached, without the bytes. */
export type AttachmentView = {
  filename: string | null
  content_type: string | null
  content_id: string | null
  /** `content` for inline bytes, `path` for a URL mock-resend deliberately never fetched. */
  source: "content" | "path"
  bytes?: number
  path?: string
}

export type TagView = {
  name: string
  value: string
}

/**
 * A captured message, in full.
 *
 * Includes the parts Resend's own retrieve response leaves out — headers, tags,
 * attachment metadata — because inspecting those is the reason the console exists.
 */
export type CapturedEmail = {
  id: string
  message_id: string | null
  from: string
  to: string[]
  cc: string[]
  bcc: string[]
  reply_to: string[]
  subject: string
  html: string | null
  text: string | null
  headers: Record<string, string>
  tags: TagView[]
  attachments: AttachmentView[]
  template: string | null
  scheduled_at: string | null
  created_at: string
  last_event: string
  /** `api` or `smtp`. */
  origin: string
  /** `POST /emails` or `SMTP`. */
  origin_label: string
  api_key: string
}

export type EmailListResponse = {
  count: number
  data: CapturedEmail[]
}

export type Stats = {
  api_key: string
  captured: number
  by_event: Record<string, number>
  webhook_endpoints: number
  fault_rules: number
}

export type WebhookView = {
  id: string
  url: string
  events: string[]
  signing_secret: string
  created_at: string
}

export type WebhookListResponse = {
  count: number
  data: WebhookView[]
}

export type ResetReport = {
  scope: string
}
