import { Link, useParams } from "@tanstack/react-router"
import { ArrowLeft, Check, Copy, Mail } from "lucide-react"
import * as React from "react"
import { toast } from "sonner"

import { Failure, Page } from "@/components/layout/page"
import { EventBadge } from "@/components/mail/event-badge"
import { MailBody } from "@/components/mail/mail-body"
import { Skeleton } from "@/components/ui/skeleton"
import { useAdmin } from "@/hooks/use-admin"
import { admin } from "@/lib/admin"
import { addressName, bareAddress, formatExact, formatRelative } from "@/lib/format"
import type { CapturedEmail } from "@/types/admin"

/** A labelled value in the header strip. */
function Field({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div className="min-w-0">
      <div className="text-[11px] text-muted-foreground/70 uppercase tracking-wide">{label}</div>
      <div className="mt-1 truncate text-[13px]">{children}</div>
    </div>
  )
}

/** Copy something a developer is about to paste into a terminal or a test. */
function CopyValue({ value }: { value: string }) {
  const [copied, setCopied] = React.useState(false)

  const copy = async () => {
    try {
      await navigator.clipboard.writeText(value)
      setCopied(true)
      window.setTimeout(() => setCopied(false), 1200)
    } catch {
      toast.error("The browser would not allow copying.")
    }
  }

  return (
    <button
      aria-label="Copy"
      className="group flex w-full items-center gap-2 rounded-md border border-border/50 bg-muted/20 px-2 py-1 text-left transition-colors hover:border-border hover:bg-muted/40"
      onClick={copy}
      type="button"
    >
      <span className="min-w-0 flex-1 truncate font-mono text-[11.5px]">{value}</span>
      {copied ? (
        <Check aria-hidden="true" className="size-3 shrink-0" strokeWidth={2} />
      ) : (
        <Copy
          aria-hidden="true"
          className="size-3 shrink-0 text-muted-foreground/60 group-hover:text-foreground"
          strokeWidth={1.75}
        />
      )}
    </button>
  )
}

export function MessagePage() {
  const { id } = useParams({ from: "/mail/$id" })

  // Polls, so an event driven from the admin API or a delivery delay shows up without a
  // reload. The body itself does not change, but `last_event` does.
  const message = useAdmin(`message:${id}`, () => admin.email(id), { pollMs: 2000 })

  return (
    <Page>
      <Link
        className="mb-5 inline-flex items-center gap-1.5 text-[13px] text-muted-foreground transition-colors hover:text-foreground"
        to="/mail"
      >
        <ArrowLeft aria-hidden="true" className="size-3.5" strokeWidth={1.75} />
        Mail
      </Link>

      {message.isLoading ? (
        <Skeleton className="h-96 w-full" />
      ) : message.error ? (
        <Failure
          detail={message.error.message}
          title={message.error.status === 404 ? "No such message" : "Could not read the message"}
        />
      ) : message.data ? (
        <Detail email={message.data} />
      ) : null}
    </Page>
  )
}

function Detail({ email }: { email: CapturedEmail }) {
  const recipient = email.to[0] ?? ""

  return (
    <div className="space-y-7">
      <header className="flex items-start gap-4">
        <div className="flex size-12 shrink-0 items-center justify-center rounded-xl border border-border/60 bg-muted/30">
          <Mail aria-hidden="true" className="size-5 text-muted-foreground" strokeWidth={1.5} />
        </div>

        <div className="min-w-0 flex-1">
          <div className="text-[12px] text-muted-foreground">Email</div>
          <h1 className="truncate font-medium text-[20px] tracking-tight" title={recipient}>
            {recipient === "" ? "no recipient" : recipient}
          </h1>
        </div>

        <EventBadge className="mt-1 px-2 py-1 text-[12px]" event={email.last_event} />
      </header>

      <div className="grid grid-cols-2 gap-x-8 gap-y-5 lg:grid-cols-4">
        <Field label="From">
          <span title={bareAddress(email.from)}>{addressName(email.from)}</span>
        </Field>
        <Field label="Subject">
          {email.subject === "" ? (
            <span className="text-muted-foreground italic">no subject</span>
          ) : (
            email.subject
          )}
        </Field>
        <Field label="To">
          <span title={email.to.map(bareAddress).join(", ")}>{email.to.join(", ")}</span>
        </Field>
        <Field label="ID">
          <CopyValue value={email.id} />
        </Field>
      </div>

      <div className="grid grid-cols-2 gap-x-8 gap-y-5 lg:grid-cols-4">
        <Field label="Log">
          <span className="font-mono text-[12px]">{email.origin_label}</span>
        </Field>
        <Field label="API key">
          <Link
            className="font-mono text-[12px] text-muted-foreground transition-colors hover:text-foreground"
            search={{ api_key: email.api_key }}
            title="Show only this key's mail"
            to="/mail"
          >
            {email.api_key}
          </Link>
        </Field>
        <Field label="Captured">
          <span title={formatExact(email.created_at)}>{formatRelative(email.created_at)}</span>
        </Field>
        {email.scheduled_at ? (
          <Field label="Scheduled for">
            <span title={formatExact(email.scheduled_at)}>{formatExact(email.scheduled_at)}</span>
          </Field>
        ) : (
          <Field label="Message-ID">
            <span className="truncate font-mono text-[11.5px] text-muted-foreground">
              {email.message_id ?? "none"}
            </span>
          </Field>
        )}
      </div>

      {email.cc.length > 0 || email.bcc.length > 0 || email.reply_to.length > 0 ? (
        <div className="grid grid-cols-2 gap-x-8 gap-y-5 lg:grid-cols-4">
          {email.cc.length > 0 ? <Field label="Cc">{email.cc.join(", ")}</Field> : null}
          {email.bcc.length > 0 ? <Field label="Bcc">{email.bcc.join(", ")}</Field> : null}
          {email.reply_to.length > 0 ? (
            <Field label="Reply-To">{email.reply_to.join(", ")}</Field>
          ) : null}
        </div>
      ) : null}

      {email.tags.length > 0 ? (
        <div>
          <div className="mb-2 text-[11px] text-muted-foreground/70 uppercase tracking-wide">
            Tags
          </div>
          <div className="flex flex-wrap gap-1.5">
            {email.tags.map((tag) => (
              <span
                className="rounded-md bg-muted px-2 py-1 font-mono text-[11.5px]"
                key={`${tag.name}=${tag.value}`}
              >
                {tag.name}: {tag.value}
              </span>
            ))}
          </div>
        </div>
      ) : null}

      {Object.keys(email.headers).length > 0 ? (
        <div>
          <div className="mb-2 text-[11px] text-muted-foreground/70 uppercase tracking-wide">
            Headers
          </div>
          <div className="space-y-1">
            {Object.entries(email.headers).map(([name, value]) => (
              <div className="flex gap-3 text-[12px]" key={name}>
                <span className="w-56 shrink-0 truncate font-mono text-muted-foreground">
                  {name}
                </span>
                <span className="min-w-0 break-all font-mono">{value}</span>
              </div>
            ))}
          </div>
        </div>
      ) : null}

      <MailBody email={email} />
    </div>
  )
}
