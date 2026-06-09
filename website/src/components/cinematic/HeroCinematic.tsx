// Cinematic hero: full-viewport inset video card, navbar pill hanging from
// the top edge, giant wordmark bottom-left, copy + install CTA bottom-right.
import { motion } from 'framer-motion';
import { ArrowRight } from 'lucide-react';
import { WordsPullUp } from './pullup';

const EASE = [0.16, 1, 0.3, 1] as const;

const NAV_ITEMS = [
  { label: 'Demo', href: '#features' },
  { label: 'Features', href: '#features' },
  { label: 'Open source', href: '/open-source' },
  { label: 'Privacy', href: '/privacy' },
  { label: 'GitHub', href: 'https://github.com/rekody/rekody' },
];

export default function HeroCinematic({ version }: { version: string }) {
  return (
    <section className="h-screen w-full bg-black p-4 md:p-6">
      <div className="relative h-full w-full overflow-hidden rounded-2xl md:rounded-[2rem]">
        <video
          className="absolute inset-0 h-full w-full object-cover"
          src="/demo.mp4"
          autoPlay
          loop
          muted
          playsInline
          // The demo opens on a "rekody." title card — skip past it so the
          // hero wordmark isn't doubled on first paint.
          onLoadedMetadata={(e) => {
            e.currentTarget.currentTime = 7;
          }}
        />
        <div className="noise-overlay pointer-events-none absolute inset-0 opacity-[0.7] mix-blend-overlay" />
        <div className="pointer-events-none absolute inset-0 bg-gradient-to-b from-black/40 via-transparent to-black/70" />

        {/* Navbar pill */}
        <nav className="absolute top-0 left-1/2 -translate-x-1/2">
          <div className="flex items-center gap-3 rounded-b-2xl bg-black px-4 py-2 sm:gap-6 md:gap-12 md:rounded-b-3xl md:px-8 lg:gap-14">
            {NAV_ITEMS.map((item) => (
              <a
                key={item.label}
                href={item.href}
                className="text-[10px] whitespace-nowrap transition-colors sm:text-xs md:text-sm"
                style={{ color: 'rgba(251, 250, 244, 0.8)' }}
                onMouseEnter={(e) => (e.currentTarget.style.color = '#FBFAF4')}
                onMouseLeave={(e) =>
                  (e.currentTarget.style.color = 'rgba(251, 250, 244, 0.8)')
                }
              >
                {item.label}
              </a>
            ))}
          </div>
        </nav>

        {/* Bottom content */}
        <div className="absolute right-0 bottom-0 left-0 px-6 pb-6 md:px-10 md:pb-8">
          <div className="flex flex-col items-start gap-6 lg:flex-row lg:items-end lg:justify-between">
            <h1
              className="text-[26vw] leading-[0.85] font-medium tracking-[-0.06em] sm:text-[24vw] md:text-[22vw] lg:text-[20vw] xl:text-[19vw]"
              style={{ color: '#FBFAF4', fontFamily: 'var(--font-serif)' }}
            >
              <WordsPullUp text="rekody" accentDot />
            </h1>
            <div className="flex max-w-md flex-col gap-5 pb-2 lg:pb-6">
              <motion.p
                initial={{ y: 20, opacity: 0 }}
                animate={{ y: 0, opacity: 1 }}
                transition={{ delay: 0.5, duration: 0.8, ease: EASE }}
                className="text-xs sm:text-sm md:text-base"
                style={{ color: 'rgba(251, 250, 244, 0.7)', lineHeight: 1.4 }}
              >
                Free, open-source voice dictation for your Mac. Hold ⌥Space,
                speak, release — your words land at the cursor in any app,
                transcribed on your machine while you talk.
              </motion.p>
              <motion.a
                initial={{ y: 20, opacity: 0 }}
                animate={{ y: 0, opacity: 1 }}
                transition={{ delay: 0.7, duration: 0.8, ease: EASE }}
                href="https://github.com/rekody/rekody#install"
                className="group flex w-fit items-center gap-2 rounded-full py-2 pr-2 pl-5 font-medium transition-all hover:gap-3"
                style={{ backgroundColor: '#FBFAF4', color: '#0F1717' }}
              >
                <span
                  className="text-sm sm:text-base"
                  style={{ fontFamily: 'var(--font-mono)' }}
                >
                  brew install rekody
                </span>
                <span
                  className="flex h-9 w-9 items-center justify-center rounded-full transition-transform group-hover:scale-110 sm:h-10 sm:w-10"
                  style={{ backgroundColor: '#0F1717' }}
                >
                  <ArrowRight size={18} style={{ color: '#FBFAF4' }} />
                </span>
              </motion.a>
              <motion.span
                initial={{ opacity: 0 }}
                animate={{ opacity: 1 }}
                transition={{ delay: 1.0, duration: 0.8 }}
                className="text-[11px]"
                style={{
                  color: 'rgba(251, 250, 244, 0.45)',
                  fontFamily: 'var(--font-mono)',
                }}
              >
                v{version} · macOS · MIT
              </motion.span>
            </div>
          </div>
        </div>
      </div>
    </section>
  );
}
