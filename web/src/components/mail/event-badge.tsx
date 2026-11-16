import { isFailureEvent, isPendingEvent } from "@/lib/format"
import { cn } from "@/lib/utils"

/**
 * A delivery event, shown as the name a caller's code branches on.
 *
 * The event string itself is always the label — never a friendlier synonym. `canceled`
 * reads oddly with one `l`, and that is exactly the spelling Resend sends, so changing
 * it here would teach someone the wrong string to match on.
 */
export function EventBadge({ event, className }: { event: string; className?: string }) {
  return (
    <span
      className={cn(
        "inline-flex shrink-0 items-center rounded-md px-1.5 py-0.5 font-medium text-[11px]",
        isFailureEvent(event)
          ? "bg-destructive/15 text-destructive"
          : isPendingEvent(event)
            ? "bg-muted text-muted-foreground"
            : "bg-emerald-500/15 text-emerald-400",
        className,
      )}
    >
      {event}
    </span>
  )
}
