import { Link } from "@tanstack/react-router"
import { Paperclip } from "lucide-react"

import { EventBadge } from "@/components/mail/event-badge"
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table"
import {
  addressName,
  bareAddress,
  formatExact,
  formatRelative,
  summariseRecipients,
} from "@/lib/format"
import type { CapturedEmail } from "@/types/admin"

/**
 * The captured mail, newest first.
 *
 * A whole row is a link rather than a click handler, so the browser's own affordances
 * work: middle-click opens a message in a tab, and the status bar shows where it goes.
 */
export function MailTable({
  emails,
  showKey = true,
}: {
  emails: CapturedEmail[]
  showKey?: boolean
}) {
  return (
    <Table>
      <TableHeader>
        <TableRow className="hover:bg-transparent">
          <TableHead className="w-[150px]">From</TableHead>
          <TableHead className="w-[150px]">To</TableHead>
          <TableHead>Subject</TableHead>
          {showKey ? <TableHead className="w-[170px]">API key</TableHead> : null}
          <TableHead className="w-[90px]">Event</TableHead>
          <TableHead className="w-[90px] text-right">Captured</TableHead>
        </TableRow>
      </TableHeader>

      <TableBody>
        {emails.map((email) => (
          <TableRow className="group cursor-pointer" key={email.id}>
            <TableCell className="p-0">
              <Link
                className="block truncate px-3 py-2.5 text-[13px]"
                params={{ id: email.id }}
                title={bareAddress(email.from)}
                to="/mail/$id"
              >
                {addressName(email.from)}
              </Link>
            </TableCell>

            <TableCell className="p-0">
              <Link
                className="block truncate px-3 py-2.5 text-[13px] text-muted-foreground"
                params={{ id: email.id }}
                title={email.to.map(bareAddress).join(", ")}
                to="/mail/$id"
              >
                {summariseRecipients(email.to)}
              </Link>
            </TableCell>

            <TableCell className="p-0">
              <Link
                className="flex items-center gap-2 px-3 py-2.5"
                params={{ id: email.id }}
                to="/mail/$id"
              >
                <span className="min-w-0 truncate text-[13px]">
                  {email.subject === "" ? (
                    <span className="text-muted-foreground italic">no subject</span>
                  ) : (
                    email.subject
                  )}
                </span>
                {email.attachments.length > 0 ? (
                  <span
                    className="flex shrink-0 items-center gap-0.5 text-[11px] text-muted-foreground"
                    title={`${email.attachments.length} attachment(s)`}
                  >
                    <Paperclip aria-hidden="true" className="size-3" strokeWidth={1.75} />
                    {email.attachments.length}
                  </span>
                ) : null}
              </Link>
            </TableCell>

            {showKey ? (
              <TableCell className="p-0">
                <Link
                  className="block truncate px-3 py-2.5 font-mono text-[11.5px] text-muted-foreground"
                  params={{ id: email.id }}
                  title={email.api_key}
                  to="/mail/$id"
                >
                  {email.api_key}
                </Link>
              </TableCell>
            ) : null}

            <TableCell className="p-0">
              <Link className="block px-3 py-2.5" params={{ id: email.id }} to="/mail/$id">
                <EventBadge event={email.last_event} />
              </Link>
            </TableCell>

            <TableCell className="p-0 text-right">
              <Link
                className="block px-3 py-2.5 text-[12px] text-muted-foreground tabular-nums"
                params={{ id: email.id }}
                title={formatExact(email.created_at)}
                to="/mail/$id"
              >
                {formatRelative(email.created_at)}
              </Link>
            </TableCell>
          </TableRow>
        ))}
      </TableBody>
    </Table>
  )
}
