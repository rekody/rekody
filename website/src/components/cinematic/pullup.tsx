// Shared text animation primitives for the cinematic landing sections.
// WordsPullUp: each word slides up into place, staggered, once in view.
// WordsPullUpMultiStyle: same motion across segments with per-segment styling
// (used to mix sans + Fraunces italic inside one heading).
import { motion, useInView } from 'framer-motion';
import { useRef } from 'react';

const EASE = [0.16, 1, 0.3, 1] as const;

export function WordsPullUp({
  text,
  className = '',
  accentDot = false,
}: {
  text: string;
  className?: string;
  /** Append the brand teal period after the last word (the "rekody." mark). */
  accentDot?: boolean;
}) {
  const ref = useRef(null);
  const inView = useInView(ref, { once: true });
  const words = text.split(' ');

  return (
    <div ref={ref} className={`inline-flex flex-wrap ${className}`}>
      {words.map((word, i) => (
        <motion.span
          key={`${word}-${i}`}
          initial={{ y: 20, opacity: 0 }}
          animate={inView ? { y: 0, opacity: 1 } : {}}
          transition={{ delay: i * 0.08, duration: 0.7, ease: EASE }}
          className="relative inline-block"
          style={{ whiteSpace: 'pre' }}
        >
          {word}
          {accentDot && i === words.length - 1 && (
            <span style={{ color: '#20808D' }}>.</span>
          )}
          {i < words.length - 1 ? ' ' : ''}
        </motion.span>
      ))}
    </div>
  );
}

export interface StyledSegment {
  text: string;
  className?: string;
}

export function WordsPullUpMultiStyle({
  segments,
  className = '',
}: {
  segments: StyledSegment[];
  className?: string;
}) {
  const ref = useRef(null);
  const inView = useInView(ref, { once: true });
  const words = segments.flatMap((seg) =>
    seg.text.split(' ').map((word) => ({ word, className: seg.className ?? '' })),
  );

  return (
    <div ref={ref} className={`inline-flex flex-wrap justify-center ${className}`}>
      {words.map(({ word, className: wordClass }, i) => (
        <motion.span
          key={`${word}-${i}`}
          initial={{ y: 20, opacity: 0 }}
          animate={inView ? { y: 0, opacity: 1 } : {}}
          transition={{ delay: i * 0.08, duration: 0.7, ease: EASE }}
          className={`inline-block ${wordClass}`}
          style={{ whiteSpace: 'pre' }}
        >
          {word}
          {i < words.length - 1 ? ' ' : ''}
        </motion.span>
      ))}
    </div>
  );
}
