// § F1.6 — translation dictionaries.
//
// One file per locale, but we keep them adjacent here so the types stay in
// sync. v1 covers the high-visibility surfaces only: app header, sidebar
// nav, route titles, common buttons, empty states, settings labels. Other
// strings (longer hints, error messages, in-page descriptions) stay
// hardcoded and will be moved over in follow-up passes.
//
// Keys use dot-notation namespaced by area. Values are template strings
// understood by `@solid-primitives/i18n::translator` — `{{var}}` for
// interpolation, plural via the chained-fn pattern (not used yet).

export type Locale = 'en' | 'fr';

export const SUPPORTED_LOCALES: ReadonlyArray<{ code: Locale; label: string }> = [
  { code: 'en', label: 'English' },
  { code: 'fr', label: 'Français' },
];

export const DEFAULT_LOCALE: Locale = 'en';

// Each leaf is just `string` — the structure is enforced by the explicit
// `Dict` type below so the FR mirror can use real translations without
// fighting literal-type narrowing from `as const`.
type Dict = {
  app: Record<
    'brand' | 'surface' | 'no_token' | 'no_token_hint' | 'toggle_theme' | 'keyboard_shortcuts' | 'toggle_nav',
    string
  >;
  nav: Record<
    | 'dashboard'
    | 'fleet'
    | 'memory'
    | 'schedules'
    | 'recipes'
    | 'skills'
    | 'analytics'
    | 'mcp'
    | 'settings',
    string
  >;
  common: Record<
    'cancel' | 'reload' | 'reset' | 'clear' | 'quit' | 'loading' | 'select_all' | 'select_none' | 'selected',
    string
  >;
  error: Record<'title', string>;
  empty: Record<
    | 'no_tasks_title'
    | 'no_tasks_hint'
    | 'no_memories_title'
    | 'no_memories_hint'
    | 'no_schedules_title'
    | 'no_schedules_hint'
    | 'no_fleet_title'
    | 'no_fleet_hint'
    | 'no_fleet_cta',
    string
  >;
  memory: Record<
    'bulk_promote' | 'bulk_promote_busy' | 'bulk_forget' | 'bulk_forget_busy' | 'bulk_forget_confirm',
    string
  >;
  settings: Record<
    | 'title'
    | 'desc'
    | 'appearance'
    | 'appearance_desc'
    | 'defaults'
    | 'defaults_desc'
    | 'notifications'
    | 'notifications_desc'
    | 'hotkeys'
    | 'about'
    | 'language'
    | 'language_desc'
    | 'enable_notifications'
    | 'theme_label'
    | 'theme_light'
    | 'theme_dark'
    | 'theme_system'
    | 'accent_label'
    | 'density_label'
    | 'density_comfortable'
    | 'density_compact'
    | 'default_sandbox'
    | 'default_routing'
    | 'default_max_steps'
    | 'default_max_steps_hint',
    string
  >;
};

export const en: Dict = {
  app: {
    brand: 'jarvis',
    surface: 'web',
    no_token: 'no token — append',
    no_token_hint: 'to the URL',
    toggle_theme: 'Toggle theme (t)',
    keyboard_shortcuts: 'Keyboard shortcuts (?)',
    toggle_nav: 'Toggle navigation',
  },
  nav: {
    dashboard: 'Dashboard',
    fleet: 'Fleet',
    memory: 'Memory',
    schedules: 'Schedules',
    recipes: 'Recipes',
    skills: 'Skills',
    analytics: 'Analytics',
    mcp: 'MCP',
    settings: 'Settings',
  },
  common: {
    cancel: 'Cancel',
    reload: 'Reload',
    reset: 'Reset',
    clear: 'Clear',
    quit: 'Quit',
    loading: 'loading…',
    select_all: 'Select all',
    select_none: 'Select none',
    selected: 'selected',
  },
  error: {
    title: 'Something went wrong',
  },
  empty: {
    no_tasks_title: 'No tasks yet',
    no_tasks_hint:
      'Submit your first goal below — jarvis runs them in the background and you can close the window any time.',
    no_memories_title: 'No active memories',
    no_memories_hint:
      'When jarvis completes a task, it proposes patterns and facts it learned. Promote them here to keep them in mind for future tasks.',
    no_schedules_title: 'No scheduled tasks',
    no_schedules_hint:
      'Recurring tasks let jarvis run audits, syncs, and checks on its own — create your first cron job with the form above.',
    no_fleet_title: 'No fleet activity',
    no_fleet_hint:
      'The fleet graph appears once you have running or recently completed tasks with parent/child relationships.',
    no_fleet_cta: 'Go to dashboard',
  },
  memory: {
    bulk_promote: 'Promote selected',
    bulk_promote_busy: 'Promoting…',
    bulk_forget: 'Forget selected',
    bulk_forget_busy: 'Forgetting…',
    bulk_forget_confirm: 'Forget the selected candidate memories?',
  },
  settings: {
    title: 'Settings',
    desc: 'Centralized preferences. Edits apply live and persist across reloads.',
    appearance: 'Appearance',
    appearance_desc: 'Theme, accent color, density.',
    defaults: 'Defaults',
    defaults_desc: 'Pre-filled values for new tasks. Per-task overrides still win.',
    notifications: 'Notifications',
    notifications_desc:
      'OS-level pings when a task finishes. Uses the Tauri plugin on desktop, the browser Notification API on web.',
    hotkeys: 'Hotkeys',
    about: 'About',
    language: 'Language',
    language_desc: 'Interface language. Reload may be needed for some legacy strings.',
    enable_notifications: 'Enable task notifications',
    theme_label: 'Theme',
    theme_light: 'light',
    theme_dark: 'dark',
    theme_system: 'system',
    accent_label: 'Accent color',
    density_label: 'Density',
    density_comfortable: 'comfortable',
    density_compact: 'compact',
    default_sandbox: 'Default sandbox',
    default_routing: 'Default routing',
    default_max_steps: 'Default max steps',
    default_max_steps_hint: 'Hard cap on agent iterations (0 = daemon default).',
  },
};

export const fr: Dict = {
  app: {
    brand: 'jarvis',
    surface: 'web',
    no_token: 'pas de token — ajoute',
    no_token_hint: "à l'URL",
    toggle_theme: 'Changer de thème (t)',
    keyboard_shortcuts: 'Raccourcis clavier (?)',
    toggle_nav: 'Basculer la navigation',
  },
  nav: {
    dashboard: 'Tableau de bord',
    fleet: 'Flotte',
    memory: 'Mémoire',
    schedules: 'Planifications',
    recipes: 'Recettes',
    skills: 'Compétences',
    analytics: 'Analyses',
    mcp: 'MCP',
    settings: 'Paramètres',
  },
  common: {
    cancel: 'Annuler',
    reload: 'Recharger',
    reset: 'Réinitialiser',
    clear: 'Effacer',
    quit: 'Quitter',
    loading: 'chargement…',
    select_all: 'Tout sélectionner',
    select_none: 'Tout désélectionner',
    selected: 'sélectionné(s)',
  },
  error: {
    title: "Une erreur s'est produite",
  },
  empty: {
    no_tasks_title: 'Aucune tâche',
    no_tasks_hint:
      'Soumets ton premier objectif ci-dessous — jarvis les exécute en arrière-plan, tu peux fermer la fenêtre à tout moment.',
    no_memories_title: 'Aucune mémoire active',
    no_memories_hint:
      "Quand jarvis termine une tâche, il propose des patterns et faits qu'il a appris. Promeus-les ici pour les garder à l'esprit dans les tâches futures.",
    no_schedules_title: 'Aucune tâche planifiée',
    no_schedules_hint:
      'Les tâches récurrentes permettent à jarvis de lancer audits, syncs et checks de manière autonome — crée ton premier cron via le formulaire ci-dessus.',
    no_fleet_title: 'Aucune activité dans la flotte',
    no_fleet_hint:
      "Le graphe de la flotte apparaît une fois que tu as des tâches en cours ou récemment terminées avec des relations parent/enfant.",
    no_fleet_cta: 'Aller au tableau de bord',
  },
  memory: {
    bulk_promote: 'Promouvoir la sélection',
    bulk_promote_busy: 'Promotion en cours…',
    bulk_forget: 'Oublier la sélection',
    bulk_forget_busy: 'Oubli en cours…',
    bulk_forget_confirm: 'Oublier les mémoires candidates sélectionnées ?',
  },
  settings: {
    title: 'Paramètres',
    desc: 'Préférences centralisées. Les modifications sont appliquées en direct et persistent au rechargement.',
    appearance: 'Apparence',
    appearance_desc: 'Thème, couleur d\'accent, densité.',
    defaults: 'Valeurs par défaut',
    defaults_desc:
      'Valeurs pré-remplies pour les nouvelles tâches. Les surcharges par tâche restent prioritaires.',
    notifications: 'Notifications',
    notifications_desc:
      "Notifications OS quand une tâche se termine. Utilise le plugin Tauri sur desktop, l'API Notification du navigateur sur web.",
    hotkeys: 'Raccourcis clavier',
    about: 'À propos',
    language: 'Langue',
    language_desc:
      "Langue de l'interface. Un rechargement peut être nécessaire pour certains textes non migrés.",
    enable_notifications: 'Activer les notifications de tâches',
    theme_label: 'Thème',
    theme_light: 'clair',
    theme_dark: 'sombre',
    theme_system: 'système',
    accent_label: "Couleur d'accent",
    density_label: 'Densité',
    density_comfortable: 'confortable',
    density_compact: 'compact',
    default_sandbox: 'Sandbox par défaut',
    default_routing: 'Routage par défaut',
    default_max_steps: 'Pas max par défaut',
    default_max_steps_hint: "Plafond strict d'itérations agent (0 = défaut daemon).",
  },
} as const;

export type { Dict };
export const DICTS: Record<Locale, Dict> = { en, fr };
