import type { CSSProperties, ReactNode } from 'react'
import { CARD_PALETTES, type CardPalette } from '../../styles/palettes'

interface CardContainerProps {
  /** One of the CARD_PALETTES keys. Drives bg / border / accent / fg. */
  palette: CardPalette
  /** Title shown as the card's bold header line. Omit for chrome-free containers. */
  title?: string
  /** Optional `aria-label` for the enclosing section — defaults to `title`. */
  ariaLabel?: string
  /** data-* attributes the card needs (e.g. data-blocker-kind, data-discovery-approval). */
  dataAttrs?: Record<string, string>
  /** When set, renders the given ARIA role on the section. */
  role?: 'alert' | 'group' | 'region' | 'status'
  /** Optional aria-live politeness; pair with role="status" or role="alert". */
  ariaLive?: 'off' | 'polite' | 'assertive'
  children: ReactNode
  /** Optional per-card style extensions merged over the palette defaults. */
  style?: CSSProperties
}

/**
 * Shared card container chrome (margin, padding, border, border-left
 * accent, rounded corners) used by every interactive turn card.
 * Consumers pick a palette name rather than hardcoding hex literals.
 */
export function CardContainer({
  palette,
  title,
  ariaLabel,
  dataAttrs,
  role,
  ariaLive,
  children,
  style,
}: CardContainerProps): JSX.Element {
  const p = CARD_PALETTES[palette]
  return (
    <section
      role={role}
      aria-label={ariaLabel ?? title}
      aria-live={ariaLive}
      {...(dataAttrs ?? {})}
      style={{
        marginTop: '0.75rem',
        padding: '0.85rem 1rem',
        background: p.bg,
        border: `1px solid ${p.border}`,
        borderLeft: `4px solid ${p.accent}`,
        borderRadius: 8,
        ...style,
      }}
    >
      {title && (
        <strong
          style={{
            display: 'block',
            fontSize: '0.85rem',
            color: p.fg,
            marginBottom: 6,
          }}
        >
          {title}
        </strong>
      )}
      {children}
    </section>
  )
}
