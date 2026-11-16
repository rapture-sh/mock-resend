import { Download, ExternalLink, Paperclip } from "lucide-react"
import * as React from "react"

import { Skeleton } from "@/components/ui/skeleton"
import { admin } from "@/lib/admin"
import { formatBytes } from "@/lib/format"
import { cn } from "@/lib/utils"
import type { CapturedEmail } from "@/types/admin"

type Format = "preview" | "text" | "html" | "raw" | "attachments"

type Tab = {
  id: Format
  label: string
  /** Hidden when the message has nothing of this kind. */
  available: (email: CapturedEmail) => boolean
  /** A count beside the label, where one helps. */
  badge?: (email: CapturedEmail) => number | undefined
}

const TABS: Tab[] = [
  { id: "preview", label: "Preview", available: (email) => email.html !== null },
  { id: "text", label: "Plain Text", available: (email) => email.text !== null },
  { id: "html", label: "HTML", available: (email) => email.html !== null },
  { id: "raw", label: "Raw", available: () => true },
  {
    id: "attachments",
    label: "Attachments",
    available: (email) => email.attachments.length > 0,
    badge: (email) => email.attachments.length,
  },
]

/**
 * The message, in whichever form you need to look at it.
 *
 * Four of these are the same bytes seen differently, which is the point: Preview is for
 * checking a design, HTML and Plain Text are for checking what the template produced,
 * and Raw is for when the answer is in a header.
 */
export function MailBody({ email }: { email: CapturedEmail }) {
  const tabs = TABS.filter((tab) => tab.available(email))
  const first = tabs[0]?.id ?? "raw"

  const [format, setFormat] = React.useState<Format>(first)

  // A different message may not have the tab the last one was showing.
  const active = tabs.some((tab) => tab.id === format) ? format : first

  return (
    <div className="overflow-hidden rounded-xl border border-border/60">
      <div className="flex items-center gap-1 border-border/60 border-b px-2 py-2">
        {tabs.map((tab) => {
          const count = tab.badge?.(email)

          return (
            <button
              className={cn(
                "flex items-center gap-1.5 rounded-md px-2.5 py-1 text-[13px] transition-colors",
                active === tab.id
                  ? "bg-accent text-foreground"
                  : "text-muted-foreground hover:bg-accent/50 hover:text-foreground",
              )}
              key={tab.id}
              onClick={() => setFormat(tab.id)}
              type="button"
            >
              {tab.label}
              {count === undefined ? null : (
                <span className="rounded bg-muted-foreground/20 px-1 text-[10.5px] tabular-nums">
                  {count}
                </span>
              )}
            </button>
          )
        })}
      </div>

      <Body email={email} format={active} />
    </div>
  )
}

function Body({ email, format }: { email: CapturedEmail; format: Format }) {
  switch (format) {
    case "preview":
      return <Preview html={email.html ?? ""} />
    case "text":
      return <Source text={email.text ?? ""} />
    case "html":
      return <Source text={email.html ?? ""} />
    case "raw":
      return <Raw id={email.id} />
    case "attachments":
      return <Attachments email={email} />
  }
}

/**
 * The HTML as a recipient would see it, in a sandboxed frame.
 *
 * `sandbox` with nothing enabled is doing real work here, not box-ticking. Captured mail
 * is whatever a caller sent, which in a test suite is often a half-finished template and
 * occasionally real mail someone pointed at this by mistake. The frame gets no scripts,
 * no same-origin access to the console, no forms and no top-level navigation, so looking
 * at a message cannot do anything.
 *
 * `srcDoc` rather than a URL for the same reason: the markup never becomes a document on
 * this origin.
 */
function Preview({ html }: { html: string }) {
  return (
    <div className="bg-white">
      <iframe
        className="h-[560px] w-full border-0"
        sandbox=""
        srcDoc={html}
        title="Message preview"
      />
    </div>
  )
}

/** The source, as it was sent, with nothing re-wrapped or re-indented. */
function Source({ text }: { text: string }) {
  return (
    <pre className="max-h-[560px] overflow-auto px-4 py-3.5 font-mono text-[12.5px] leading-[1.65] whitespace-pre-wrap break-words">
      {text === "" ? <span className="text-muted-foreground italic">empty</span> : text}
    </pre>
  )
}

/** The `.eml`, which is also exactly what the disk backend writes. */
function Raw({ id }: { id: string }) {
  const [raw, setRaw] = React.useState<string>()
  const [failed, setFailed] = React.useState(false)

  React.useEffect(() => {
    let current = true
    setRaw(undefined)
    setFailed(false)

    admin
      .raw(id)
      .then((text) => {
        if (current) {
          setRaw(text)
        }
      })
      .catch(() => {
        if (current) {
          setFailed(true)
        }
      })

    return () => {
      current = false
    }
  }, [id])

  if (failed) {
    return (
      <div className="px-4 py-10 text-center text-[13px] text-muted-foreground">
        The raw message could not be read.
      </div>
    )
  }

  if (raw === undefined) {
    return (
      <div className="space-y-2 px-4 py-3.5">
        <Skeleton className="h-3.5 w-2/3" />
        <Skeleton className="h-3.5 w-1/2" />
        <Skeleton className="h-3.5 w-3/4" />
      </div>
    )
  }

  return (
    <div>
      <Source text={raw} />
      <div className="border-border/60 border-t px-4 py-2">
        <a
          className="inline-flex items-center gap-1.5 text-[12px] text-muted-foreground transition-colors hover:text-foreground"
          href={admin.rawUrl(id)}
          rel="noreferrer"
          target="_blank"
        >
          <ExternalLink aria-hidden="true" className="size-3.5" strokeWidth={1.75} />
          Open the .eml
        </a>
      </div>
    </div>
  )
}

/**
 * What was attached.
 *
 * An attachment sent as a `path` has no bytes here and says so: mock-resend records the
 * URL and never fetches it, because fetching a URL the caller chose is a server-side
 * request forgery on the developer's own machine.
 */
function Attachments({ email }: { email: CapturedEmail }) {
  return (
    <div className="divide-y divide-border/60">
      {email.attachments.map((attachment, index) => (
        // The index is the identity here, not a stand-in for one: the admin API
        // addresses an attachment by position precisely because a filename is a
        // caller-supplied string that can repeat or be absent, and this list comes from
        // one immutable message, so it never reorders.
        // biome-ignore lint/suspicious/noArrayIndexKey: position is the attachment's identity in the API
        <div className="flex items-center gap-3 px-4 py-3" key={index}>
          <Paperclip
            aria-hidden="true"
            className="size-4 shrink-0 text-muted-foreground"
            strokeWidth={1.75}
          />

          <div className="min-w-0 flex-1">
            <div className="truncate text-[13px]">
              {attachment.filename ?? <span className="text-muted-foreground italic">unnamed</span>}
            </div>
            <div className="mt-0.5 truncate text-[11.5px] text-muted-foreground">
              {attachment.content_type ?? "no declared type"}
              {attachment.source === "content" && attachment.bytes !== undefined ? (
                <>
                  <span className="px-1 text-muted-foreground/40">·</span>
                  {formatBytes(attachment.bytes)}
                </>
              ) : null}
              {attachment.content_id ? (
                <>
                  <span className="px-1 text-muted-foreground/40">·</span>
                  inline as <span className="font-mono">cid:{attachment.content_id}</span>
                </>
              ) : null}
            </div>
          </div>

          {attachment.source === "content" ? (
            <a
              className="inline-flex shrink-0 items-center gap-1.5 rounded-md border border-border/60 px-2.5 py-1 text-[12px] transition-colors hover:bg-accent"
              href={admin.attachmentUrl(email.id, index, true)}
            >
              <Download aria-hidden="true" className="size-3.5" strokeWidth={1.75} />
              Download
            </a>
          ) : (
            <span
              className="shrink-0 text-[11.5px] text-muted-foreground"
              title={attachment.path ?? undefined}
            >
              sent as a URL, never fetched
            </span>
          )}
        </div>
      ))}
    </div>
  )
}
