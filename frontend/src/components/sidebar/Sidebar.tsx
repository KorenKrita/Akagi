import { useEffect, useState } from 'react'
import { Link } from 'react-router-dom'
import { useTranslation } from 'react-i18next'
import { ChevronLeft } from 'lucide-react'
import { getVersion } from '@tauri-apps/api/app'

import { cn } from '@/lib/utils'
import { Button } from '@/components/ui/button'
import {
  Tooltip,
  TooltipContent,
  TooltipProvider,
  TooltipTrigger,
} from '@/components/ui/tooltip'
import { useSidebar } from '@/hooks/useSidebar'
import { GithubMark, DiscordMark } from '@/components/BrandMarks'
import { AkagiIcon, AkagiWordmark } from '@/components/BrandLogo'
import { AKAGI_GITHUB_URL, AKAGI_DISCORD_URL, openExternal } from '@/lib/external'
import { HAS_TAURI } from '@/lib/tauri'
import { LANG_LABELS, SUPPORTED_LANGS, type SupportedLang } from '@/i18n'
import { selectHasNotifiableUpdate, useUpdaterStore } from '@/stores/updaterStore'
import { Menu } from './Menu'

// Fallback used in the browser dev preview (no Tauri runtime → no
// app.getVersion). Lines up with Cargo.toml so devs see a sensible
// string until the real version resolves.
const VERSION_FALLBACK = '3.3.0'

export function Sidebar() {
  const { t, i18n } = useTranslation()
  const isOpen = useSidebar((s) => s.isOpen)
  const toggleOpen = useSidebar((s) => s.toggleOpen)
  const setIsHover = useSidebar((s) => s.setIsHover)
  const isHover = useSidebar((s) => s.isHover)
  const settings = useSidebar((s) => s.settings)
  const hasUpdate = useUpdaterStore(selectHasNotifiableUpdate)
  const openUpdateDialog = useUpdaterStore((s) => s.openDialog)
  const [version, setVersion] = useState(VERSION_FALLBACK)
  useEffect(() => {
    if (!HAS_TAURI) return
    getVersion().then(setVersion).catch(() => {})
  }, [])
  // `open` includes the transient hover-open state. Only `isOpen` (pinned)
  // affects main content margin in App.tsx — hover-open expands the sidebar
  // visually as an overlay above main, so width-sensitive widgets like
  // react-grid-layout don't thrash on every cursor pass.
  const open = isOpen || (settings.isHoverOpen && isHover)

  return (
    <aside
      className={cn(
        'fixed top-0 left-0 z-20 h-screen -translate-x-full lg:translate-x-0 transition-[width] ease-in-out duration-300',
        open ? 'w-[18rem]' : 'w-[5.625rem]',
        settings.disabled && 'hidden',
      )}
    >
      <div
        onMouseEnter={() => setIsHover(true)}
        onMouseLeave={() => setIsHover(false)}
        className="relative h-full flex flex-col px-3 py-4 overflow-hidden bg-sidebar text-sidebar-foreground border-r border-border shadow-md dark:shadow-zinc-800"
      >
        <div
          className={cn(
            'flex items-center mb-2 shrink-0',
            open ? 'justify-between gap-2' : 'justify-center',
          )}
        >
          <Link
            to="/"
            className={cn(
              'flex items-center gap-2 rounded-md px-1 py-1 hover:opacity-80 transition-opacity',
              !open && 'justify-center',
            )}
            aria-label="Akagi"
          >
            {open ? (
              <span className="flex items-center gap-1.5 whitespace-nowrap">
                <AkagiWordmark className="h-7" />
                <span className="text-xs text-muted-foreground font-normal">V3</span>
              </span>
            ) : (
              <AkagiIcon className="h-7" />
            )}
          </Link>
          {open && (
            <Button
              variant="ghost"
              size="icon"
              onClick={toggleOpen}
              className="h-7 w-7 text-muted-foreground hover:text-foreground"
              aria-label={isOpen ? t('sidebar.collapse') : t('sidebar.pin')}
            >
              <ChevronLeft
                className={cn(
                  'h-4 w-4 transition-transform duration-300',
                  !isOpen && 'rotate-180',
                )}
              />
            </Button>
          )}
        </div>
        <Menu isOpen={open} />
        {/* Community footer — always rendered, even when the sidebar is
            collapsed, so the GitHub / Discord links are reachable in
            both states. The version + language picker rides along
            below it but only when there's room (open state). */}
        <div
          className={cn(
            'mt-2 shrink-0 flex items-center gap-1 border-t border-border pt-3',
            open ? 'justify-start px-1' : 'justify-center',
          )}
        >
          <CommunityIconButton
            label="GitHub"
            collapsed={!open}
            onClick={() => openExternal(AKAGI_GITHUB_URL)}
          >
            <GithubMark className="h-4 w-4" />
          </CommunityIconButton>
          <CommunityIconButton
            label="Discord"
            collapsed={!open}
            onClick={() => openExternal(AKAGI_DISCORD_URL)}
          >
            <DiscordMark className="h-4 w-4" />
          </CommunityIconButton>
        </div>
        {open && (
          <div className="mt-2 shrink-0 flex items-center justify-between gap-2 text-xs text-muted-foreground">
            <button
              type="button"
              onClick={hasUpdate ? openUpdateDialog : undefined}
              className={cn(
                'flex items-center gap-1.5 rounded px-1 py-0.5',
                hasUpdate
                  ? 'cursor-pointer hover:text-foreground'
                  : 'cursor-default',
              )}
              aria-label={
                hasUpdate ? t('updates.toast.action') : undefined
              }
              disabled={!hasUpdate}
            >
              <span>v{version}</span>
              {hasUpdate && (
                <span
                  className="inline-block h-1.5 w-1.5 rounded-full bg-red-500"
                  aria-hidden="true"
                />
              )}
            </button>
            <select
              className="bg-transparent border border-border rounded px-1.5 py-0.5"
              value={i18n.language}
              onChange={(e) => void i18n.changeLanguage(e.target.value)}
            >
              {SUPPORTED_LANGS.map((lang) => (
                <option key={lang} value={lang}>
                  {LANG_LABELS[lang as SupportedLang]}
                </option>
              ))}
            </select>
          </div>
        )}
      </div>
    </aside>
  )
}

function CommunityIconButton({
  label,
  collapsed,
  onClick,
  children,
}: {
  label: string
  collapsed: boolean
  onClick: () => void
  children: React.ReactNode
}) {
  // Only show the right-side tooltip when the sidebar is collapsed —
  // when expanded, the button sits in plain view and tooltips would
  // just be visual noise.
  return (
    <TooltipProvider disableHoverableContent>
      <Tooltip delayDuration={100}>
        <TooltipTrigger asChild>
          <Button
            variant="ghost"
            size="icon"
            className="h-9 w-9 text-muted-foreground hover:text-foreground"
            onClick={onClick}
            aria-label={label}
          >
            {children}
          </Button>
        </TooltipTrigger>
        {collapsed && <TooltipContent side="right">{label}</TooltipContent>}
      </Tooltip>
    </TooltipProvider>
  )
}
