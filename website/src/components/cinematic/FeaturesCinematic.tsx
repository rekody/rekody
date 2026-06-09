// Features: 4-card grid — one video card + three numbered checklist cards.
import { motion, useInView } from 'framer-motion';
import { ArrowRight, AudioWaveform, Check, ShieldCheck, Sparkles } from 'lucide-react';
import { useRef } from 'react';
import type { ReactNode } from 'react';
import { WordsPullUpMultiStyle } from './pullup';

const EASE = [0.22, 1, 0.36, 1] as const;
const CREAM = '#FBFAF4';
const CARD_BG = '#161D1D';

function Card({ index, children }: { index: number; children: ReactNode }) {
  const ref = useRef(null);
  const inView = useInView(ref, { once: true, margin: '-100px' });
  return (
    <motion.div
      ref={ref}
      initial={{ scale: 0.95, opacity: 0 }}
      animate={inView ? { scale: 1, opacity: 1 } : {}}
      transition={{ delay: index * 0.15, duration: 0.7, ease: EASE }}
      className="flex flex-1 flex-col overflow-hidden rounded-xl"
      style={{ backgroundColor: CARD_BG }}
    >
      {children}
    </motion.div>
  );
}

function ChecklistCard({
  index,
  number,
  title,
  icon,
  items,
  href,
}: {
  index: number;
  number: string;
  title: string;
  icon: ReactNode;
  items: string[];
  href: string;
}) {
  return (
    <Card index={index}>
      <div className="flex h-full flex-col gap-5 p-6">
        <div
          className="flex h-10 w-10 items-center justify-center rounded-lg sm:h-12 sm:w-12"
          style={{ backgroundColor: 'rgba(32, 128, 141, 0.15)', color: '#2FA3B3' }}
        >
          {icon}
        </div>
        <h3 className="text-lg font-medium" style={{ color: CREAM }}>
          {title}{' '}
          <span className="text-sm" style={{ color: '#5E6E6E' }}>
            ({number})
          </span>
        </h3>
        <ul className="flex flex-1 flex-col gap-3">
          {items.map((item) => (
            <li key={item} className="flex items-start gap-2.5">
              <Check size={16} className="mt-0.5 shrink-0" style={{ color: '#2FA3B3' }} />
              <span className="text-sm text-gray-400">{item}</span>
            </li>
          ))}
        </ul>
        <a
          href={href}
          className="group flex w-fit items-center gap-2 text-sm"
          style={{ color: CREAM }}
        >
          Learn more
          <ArrowRight
            size={16}
            className="-rotate-45 transition-transform group-hover:rotate-0"
          />
        </a>
      </div>
    </Card>
  );
}

export default function FeaturesCinematic() {
  return (
    <section id="features" className="relative min-h-screen bg-black px-4 py-20 md:px-6 md:py-28">
      <div className="bg-noise pointer-events-none absolute inset-0 opacity-[0.15]" />
      <div className="relative mx-auto flex max-w-7xl flex-col gap-14 md:gap-20">
        <h2 className="text-center text-xl font-normal sm:text-2xl md:text-3xl lg:text-4xl">
          <span className="block" style={{ color: CREAM }}>
            <WordsPullUpMultiStyle
              segments={[{ text: 'Streaming dictation for people who live in the terminal.' }]}
            />
          </span>
          <span className="block text-gray-500">
            <WordsPullUpMultiStyle
              segments={[{ text: 'Built for speed. Powered by open models.' }]}
            />
          </span>
        </h2>

        <div className="flex flex-col gap-3 md:flex-row md:flex-wrap lg:h-[480px] lg:flex-nowrap lg:gap-2">
          <Card index={0}>
            <div className="relative h-64 md:h-full">
              <video
                className="absolute inset-0 h-full w-full object-cover"
                src="/demo.mp4"
                autoPlay
                loop
                muted
                playsInline
              />
              <div className="pointer-events-none absolute inset-0 bg-gradient-to-t from-black/70 to-transparent" />
              <p
                className="absolute bottom-5 left-5 text-lg font-medium"
                style={{ color: '#FBFAF4' }}
              >
                Speak. Release. Done.
              </p>
            </div>
          </Card>

          <ChecklistCard
            index={1}
            number="01"
            title="Streaming STT."
            icon={<AudioWaveform size={22} />}
            items={[
              'Words decode while you talk — on-device NVIDIA Nemotron',
              'Final text lands ~50ms after you release the key',
              'Whisper turbo covers 100+ languages',
            ]}
            href="https://github.com/rekody/rekody#engines"
          />

          <ChecklistCard
            index={2}
            number="02"
            title="Skills."
            icon={<Sparkles size={22} />}
            items={[
              'Reshape dictation into email, notes, or commit messages',
              'Cycle skills live with ⌥Space + Tab',
              'Personal dictionary keeps your jargon intact',
            ]}
            href="https://github.com/rekody/rekody#skills"
          />

          <ChecklistCard
            index={3}
            number="03"
            title="Private by default."
            icon={<ShieldCheck size={22} />}
            items={[
              'Audio never leaves your Mac on local engines',
              'History stays local — browse it in a built-in TUI',
              'MIT-licensed, no telemetry, no account',
            ]}
            href="https://github.com/rekody/rekody"
          />
        </div>
      </div>
    </section>
  );
}
