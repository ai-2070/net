// Athena's own agents, the ones its Agent library comes with: names, emoji,
// groups and descriptions from the frontmatter of library/athena-agents/ at
// the commit below (groups in groups.json order). Regenerate when the
// library changes; "Read" opens the same file the app hires from.

export const LIBRARY_COMMIT = "29206e21cb462732b9610960b1bcbc270aac53a7";

export interface LibraryAgent {
  name: string;
  emoji: string;
  group: string;
  description: string;
  /** In the starter team "Add Athena team" hires. */
  starter: boolean;
  path: string;
}

export const LIBRARY_GROUPS: ReadonlyArray<string> = [
  "Engineering",
  "Design",
  "Images, video and 3D",
  "Games and the voxel world",
  "Marketing and social",
  "Sales and support",
  "Product and projects",
  "Research and analysis",
  "Business and finance",
];

export const LIBRARY: ReadonlyArray<LibraryAgent> = [
  {
    name: "Accessibility Auditor",
    emoji: "♿",
    group: "Engineering",
    description:
      "Audits websites, apps and documents against WCAG, Section 508 and the European Accessibility Act, puts what stops disabled people finishing a task first, and can fix it or draft the conformance report.",
    starter: false,
    path: "library/athena-agents/engineering/accessibility-auditor.md",
  },
  {
    name: "AI Code Security Auditor",
    emoji: "🔐",
    group: "Engineering",
    description:
      "Checks an app built mostly with AI tools, often on Supabase, Firebase or Vercel, for leaked keys, open databases and unguarded endpoints, explains each risk in plain words and fixes the worst first.",
    starter: false,
    path: "library/athena-agents/engineering/ai-code-security-auditor.md",
  },
  {
    name: "AI Engineer",
    emoji: "🤖",
    group: "Engineering",
    description:
      "Builds AI into products, from model API calls and answers over your own documents (RAG) to tools, agents and classic ML, measured with real evals and kept safe and affordable to run.",
    starter: false,
    path: "library/athena-agents/engineering/ai-engineer.md",
  },
  {
    name: "API Tester",
    emoji: "📡",
    group: "Engineering",
    description:
      "Tests REST, GraphQL and webhook APIs, yours or ones you depend on, against their docs under bad input, wrong users, retries and failures, then writes the tests that keep them working.",
    starter: false,
    path: "library/athena-agents/engineering/api-tester.md",
  },
  {
    name: "Application Security Engineer",
    emoji: "🛡️",
    group: "Engineering",
    description:
      "Secures an app from the inside, before a launch or after a scan: threat models, security code review, dependency risk, CI scanners, and the fix with a test that keeps it fixed.",
    starter: false,
    path: "library/athena-agents/engineering/application-security-engineer.md",
  },
  {
    name: "Backend Architect",
    emoji: "🗄️",
    group: "Engineering",
    description:
      "Designs and builds the server side, from APIs and data models to migrations, background jobs and permissions, so it holds up under load and failure and can change without breaking its clients.",
    starter: false,
    path: "library/athena-agents/engineering/backend-architect.md",
  },
  {
    name: "Code Reviewer",
    emoji: "👁️",
    group: "Engineering",
    description:
      "Reviews a change, AI-written or not, for bugs, security holes and code that will be hard to keep, says what to fix first and can fix it.",
    starter: false,
    path: "library/athena-agents/engineering/code-reviewer.md",
  },
  {
    name: "Codebase Guide",
    emoji: "🗺️",
    group: "Engineering",
    description:
      "Explains an unfamiliar codebase to its owner, a new developer or an agent about to change it: how it fits together, which files a change needs, and where it has drifted.",
    starter: false,
    path: "library/athena-agents/engineering/codebase-guide.md",
  },
  {
    name: "Data Engineer",
    emoji: "🚰",
    group: "Engineering",
    description:
      "Builds and fixes data pipelines: gets data from apps, files and APIs into a database or warehouse, shapes it into tested tables that reports can trust, and keeps it loading on schedule.",
    starter: false,
    path: "library/athena-agents/engineering/data-engineer.md",
  },
  {
    name: "Database Optimizer",
    emoji: "🛢️",
    group: "Engineering",
    description:
      "Makes Postgres, MySQL and SQLite databases fast and keeps them safe: queries, indexes, locks, connections, live schema changes, replication and backups, when an app slows down or a migration looks risky.",
    starter: false,
    path: "library/athena-agents/engineering/database-optimizer.md",
  },
  {
    name: "DevOps Automator",
    emoji: "⚙️",
    group: "Engineering",
    description:
      "Sets up and fixes how software gets built, tested and shipped: CI/CD pipelines, infrastructure as code, containers, secrets and Git branching, for a broken build, a first deploy or releases that feel risky.",
    starter: false,
    path: "library/athena-agents/engineering/devops-automator.md",
  },
  {
    name: "Frontend Developer",
    emoji: "🧩",
    group: "Engineering",
    description:
      "Builds and fixes websites and web apps in the framework the project already uses: layout, components, state, forms, accessibility and speed, checked in a real browser before it's called done.",
    starter: false,
    path: "library/athena-agents/engineering/frontend-developer.md",
  },
  {
    name: "MCP Builder",
    emoji: "🔌",
    group: "Engineering",
    description:
      "Designs, builds and tests MCP servers that give agents new tools by wrapping an API, a database, a local program or a company system, and connects them to this dashboard or Claude Code.",
    starter: false,
    path: "library/athena-agents/engineering/mcp-builder.md",
  },
  {
    name: "Minimal Change Engineer",
    emoji: "🪡",
    group: "Engineering",
    description:
      "Fixes a bug at its cause, or makes one asked-for change, in the smallest tested diff that's easy to review and revert: for hotfixes, production code, fragile codebases and other people's repos.",
    starter: false,
    path: "library/athena-agents/engineering/minimal-change-engineer.md",
  },
  {
    name: "Mobile App Builder",
    emoji: "📱",
    group: "Engineering",
    description:
      "Builds and ships iOS and Android apps in Swift, Kotlin, React Native or Flutter: features, crash fixes, signing, store review and listings, and staged rollouts that can be stopped.",
    starter: false,
    path: "library/athena-agents/engineering/mobile-app-builder.md",
  },
  {
    name: "Penetration Tester",
    emoji: "🔓",
    group: "Engineering",
    description:
      "Runs authorized security tests on your own web apps, APIs and networks the way an attacker would, proves what it finds with safe evidence and fixes, and helps with CTFs.",
    starter: false,
    path: "library/athena-agents/engineering/penetration-tester.md",
  },
  {
    name: "Performance Benchmarker",
    emoji: "⏱️",
    group: "Engineering",
    description:
      "Measures how fast software really is, from web pages and APIs to scripts and builds, finds where the time and memory go, and says honestly whether a change made a real difference.",
    starter: false,
    path: "library/athena-agents/engineering/performance-benchmarker.md",
  },
  {
    name: "Prompt Engineer",
    emoji: "🧪",
    group: "Engineering",
    description:
      "Writes, tests and fixes prompts, agent instructions, tool descriptions and skills, for products or this dashboard's agents, when a model keeps getting it wrong, and proves the fix with test cases.",
    starter: false,
    path: "library/athena-agents/engineering/prompt-engineer.md",
  },
  {
    name: "Reality Checker",
    emoji: "🧾",
    group: "Engineering",
    description:
      'Checks claims like "it works", "all tests pass" or "it\'s fixed" by running the real thing, often on other agents\' work, and says with proof what holds, what\'s unproven and what\'s broken.',
    starter: false,
    path: "library/athena-agents/engineering/reality-checker.md",
  },
  {
    name: "Secrets Hygiene Engineer",
    emoji: "🔑",
    group: "Engineering",
    description:
      "Finds leaked or badly kept API keys, passwords, tokens and certificates in code, git history, logs and CI, moves them to a proper store, and handles a leak from rotation to cleanup.",
    starter: false,
    path: "library/athena-agents/engineering/secrets-hygiene-engineer.md",
  },
  {
    name: "Security Architect",
    emoji: "🏰",
    group: "Engineering",
    description:
      "Designs and reviews security for systems on AWS, Azure, Google Cloud or your own servers: who can reach what, how data is protected, what raises an alarm, and what to fix first.",
    starter: false,
    path: "library/athena-agents/engineering/security-architect.md",
  },
  {
    name: "Senior Developer",
    emoji: "🧑‍💻",
    group: "Engineering",
    description:
      "Writes and fixes code across the stack, from a quick prototype to a careful change in a large codebase, and proves it works; the default pick for building something or fixing a bug.",
    starter: false,
    path: "library/athena-agents/engineering/senior-developer.md",
  },
  {
    name: "Site Reliability Engineer",
    emoji: "📟",
    group: "Engineering",
    description:
      "Keeps services up and fast, with alerts that mean something, service levels, enough capacity and backups that restore, and leads the response when something breaks, through to the write-up.",
    starter: false,
    path: "library/athena-agents/engineering/site-reliability-engineer.md",
  },
  {
    name: "Software Architect",
    emoji: "📐",
    group: "Engineering",
    description:
      "Designs a software system and makes the big calls: how to split it, where data lives, what to build or buy, how it will change; writes them down and critiques plans before code exists.",
    starter: false,
    path: "library/athena-agents/engineering/software-architect.md",
  },
  {
    name: "Technical Writer",
    emoji: "📝",
    group: "Engineering",
    description:
      "Writes docs people can follow: READMEs, getting-started guides, how-tos, API references, release notes, help articles and in-app text, for developers or non-technical users, with every step tested.",
    starter: false,
    path: "library/athena-agents/engineering/technical-writer.md",
  },
  {
    name: "Test Automation Engineer",
    emoji: "✅",
    group: "Engineering",
    description:
      "Writes and keeps up automated tests, from unit tests to end-to-end ones in Playwright or Cypress, fixes flaky tests and reads failing runs to say what's really broken.",
    starter: false,
    path: "library/athena-agents/engineering/test-automation-engineer.md",
  },
  {
    name: "Brand Guardian",
    emoji: "🏷️",
    group: "Design",
    description:
      "Shapes and protects a brand, from what it stands for to its voice, name, logo and colours, writes a short guide to keep it consistent, and checks new work against it.",
    starter: false,
    path: "library/athena-agents/design/brand-guardian.md",
  },
  {
    name: "UI Designer",
    emoji: "🖌️",
    group: "Design",
    description:
      "Designs how web and mobile screens look, from a quick mockup to a full component set: hierarchy, type, colour, spacing, every state, and the design system that keeps them consistent.",
    starter: false,
    path: "library/athena-agents/design/ui-designer.md",
  },
  {
    name: "UI Finish Reviewer",
    emoji: "🔬",
    group: "Design",
    description:
      "Reviews a built screen or product before it ships, in a browser at real sizes and themes, finds what looks unfinished or generic, and says exactly what to change, or changes it.",
    starter: false,
    path: "library/athena-agents/design/ui-finish-reviewer.md",
  },
  {
    name: "UX Architect",
    emoji: "🏗️",
    group: "Design",
    description:
      "Works out how a product is organised and how people move through it, from navigation and flows to forms and their rules, and hands developers a structure they can build.",
    starter: false,
    path: "library/athena-agents/design/ux-architect.md",
  },
  {
    name: "UX Researcher",
    emoji: "🔍",
    group: "Design",
    description:
      "Finds out how people really use a product and why they get stuck, through interviews, tests, surveys, feedback and analytics, and says what to change.",
    starter: false,
    path: "library/athena-agents/design/ux-researcher.md",
  },
  {
    name: "3D Artist",
    emoji: "🧊",
    group: "Images, video and 3D",
    description:
      "Builds, lights and renders 3D scenes, models and product shots in Blender, turns them into turntable videos or GIFs, and prepares models for the web, game engines or 3D printing.",
    starter: true,
    path: "library/athena-agents/media/3d-artist.md",
  },
  {
    name: "Image Maker",
    emoji: "🎨",
    group: "Images, video and 3D",
    description:
      "Makes images with your own ComfyUI workflows, from a rough idea to finished files: picks the workflow, prompts for its model, checks each result, iterates, and sizes them for where they'll go.",
    starter: true,
    path: "library/athena-agents/media/image-maker.md",
  },
  {
    name: "Video Editor",
    emoji: "🎬",
    group: "Images, video and 3D",
    description:
      "Edits your video: cuts, joins, trims pauses, adds captions and titles, fixes the sound, makes vertical clips for social and exports for each platform, mostly with ffmpeg, plus Premiere or After Effects.",
    starter: true,
    path: "library/athena-agents/media/video-editor.md",
  },
  {
    name: "Blender Add-on Engineer",
    emoji: "🧰",
    group: "Games and the voxel world",
    description:
      "Builds Blender add-ons and Python tools that take repeated work off artists: one-click operators and panels, batch exporters, asset and naming checks, headless command-line jobs, packaged as extensions and tested.",
    starter: false,
    path: "library/athena-agents/games/blender-add-on-engineer.md",
  },
  {
    name: "Game Designer",
    emoji: "🎮",
    group: "Games and the voxel world",
    description:
      "Designs how a game plays: the core loop, rules, progression, difficulty, balance, economy and fair monetisation, tested with quick prototypes, balance models and playtests before content gets built.",
    starter: false,
    path: "library/athena-agents/games/game-designer.md",
  },
  {
    name: "Godot Developer",
    emoji: "🦾",
    group: "Games and the voxel world",
    description:
      "Builds and fixes games in Godot 4, from first prototype to release: scenes, GDScript or C#, physics, UI, shaders, online multiplayer, editor tools, tests, profiling and exports, web included.",
    starter: false,
    path: "library/athena-agents/games/godot-developer.md",
  },
  {
    name: "Narrative Designer",
    emoji: "🎭",
    group: "Games and the voxel world",
    description:
      "Designs and writes the story side of games and interactive fiction (world, characters, quests, branching dialogue, barks, lore) so story and play pull together, in ink, Yarn Spinner, Twine or spreadsheets.",
    starter: false,
    path: "library/athena-agents/games/narrative-designer.md",
  },
  {
    name: "Scene Builder",
    emoji: "🪸",
    group: "Games and the voxel world",
    description:
      "Makes the Station's scene, a live web page that draws the project's Brain, teams and agents its own way (an underwater garden, planets, a night city), with three.js or a canvas, checked in the Station.",
    starter: false,
    path: "library/athena-agents/games/scene-builder.md",
  },
  {
    name: "Technical Artist",
    emoji: "🎛️",
    group: "Games and the voxel world",
    description:
      "Gets game art into Unity, Unreal or Godot looking right and running within budget: shaders, VFX, LODs, texture and export pipelines, profiling, and tools that catch broken files.",
    starter: false,
    path: "library/athena-agents/games/technical-artist.md",
  },
  {
    name: "Three.js Developer",
    emoji: "🔺",
    group: "Games and the voxel world",
    description:
      "Builds 3D on the web with three.js and React Three Fiber: product viewers and configurators, data and map scenes, browser games, WebXR and 3D inside existing apps, checked on real devices.",
    starter: false,
    path: "library/athena-agents/games/three-js-developer.md",
  },
  {
    name: "Unity Developer",
    emoji: "🕹️",
    group: "Games and the voxel world",
    description:
      "Builds games and real-time apps in Unity: C# gameplay, project structure, editor tools, UI, Netcode multiplayer, Shader Graph materials, tests, command-line builds and profiling, in the project's own Unity version.",
    starter: false,
    path: "library/athena-agents/games/unity-developer.md",
  },
  {
    name: "Unreal Developer",
    emoji: "🌌",
    group: "Games and the voxel world",
    description:
      "Builds games and real-time projects in Unreal Engine 5: C++ and Blueprints, gameplay framework, multiplayer replication, open worlds, editor tools, tests, packaged builds and profiling, checked against the project's engine version.",
    starter: false,
    path: "library/athena-agents/games/unreal-developer.md",
  },
  {
    name: "World Builder",
    emoji: "🧱",
    group: "Games and the voxel world",
    description:
      "Builds the voxel world around the Station in WORLD.md's style: terrain, buildings, props, prefab kits and agents' characters, with its light and time of day, and keeps WORLD.md up to date.",
    starter: true,
    path: "library/athena-agents/games/world-builder.md",
  },
  {
    name: "AI Search Optimizer",
    emoji: "💬",
    group: "Marketing and social",
    description:
      "Checks what ChatGPT, Claude, Gemini, Perplexity and Google's AI answers say about a business, fixes what they get wrong or leave out, and makes its site work for AI agents acting for customers.",
    starter: false,
    path: "library/athena-agents/marketing/ai-search-optimizer.md",
  },
  {
    name: "Book Co-Author",
    emoji: "📖",
    group: "Marketing and social",
    description:
      "Helps founders and experts write a nonfiction book in their own voice, from the core idea and outline to chapters drawn from their talks and notes, the proposal and publishing.",
    starter: false,
    path: "library/athena-agents/marketing/book-co-author.md",
  },
  {
    name: "Content Creator",
    emoji: "✍️",
    group: "Marketing and social",
    description:
      "Plans and writes blog posts, newsletters, guides, case studies, scripts and social posts in your brand's voice, turning one good idea into several pieces on a schedule you can keep.",
    starter: false,
    path: "library/athena-agents/marketing/content-creator.md",
  },
  {
    name: "Email Marketer",
    emoji: "📬",
    group: "Marketing and social",
    description:
      "Plans and writes your email marketing (newsletters, welcome and onboarding sequences, launches, win-backs) and looks after the list, consent, deliverability and results, setting flows up as drafts for your OK.",
    starter: false,
    path: "library/athena-agents/marketing/email-marketer.md",
  },
  {
    name: "Growth Hacker",
    emoji: "📈",
    group: "Marketing and social",
    description:
      "Finds where a product loses people on the way from first visit to paying and staying, then tests fixes to onboarding, referrals, pricing pages and channels; pick it when growth has stalled.",
    starter: false,
    path: "library/athena-agents/marketing/growth-hacker.md",
  },
  {
    name: "LinkedIn Writer",
    emoji: "💼",
    group: "Marketing and social",
    description:
      "Writes LinkedIn posts, articles and profile text in your own voice, from your real work, for founders, freelancers and small teams who don't want to sound like LinkedIn.",
    starter: false,
    path: "library/athena-agents/marketing/linkedin-writer.md",
  },
  {
    name: "Paid Social Strategist",
    emoji: "🧲",
    group: "Marketing and social",
    description:
      "Plans, runs and improves paid ads on Facebook, Instagram, TikTok, LinkedIn, Pinterest, Snapchat and Reddit, and makes the ads themselves: concepts, copy, images and short videos, judged by the sales they bring.",
    starter: false,
    path: "library/athena-agents/marketing/paid-social-strategist.md",
  },
  {
    name: "PR Manager",
    emoji: "📰",
    group: "Marketing and social",
    description:
      "Gets you or your business into the press: finds the news worth telling, pitches the right journalists and podcasts, writes releases and media kits, preps interviews and handles statements when things go wrong.",
    starter: false,
    path: "library/athena-agents/marketing/pr-manager.md",
  },
  {
    name: "Reddit Community Builder",
    emoji: "🏘️",
    group: "Marketing and social",
    description:
      "Helps you or your small business take part in Reddit honestly: finds the right subreddits, drafts answers and posts to their rules, reads what your market says, and helps run your own subreddit.",
    starter: false,
    path: "library/athena-agents/marketing/reddit-community-builder.md",
  },
  {
    name: "Search Ads Strategist",
    emoji: "🖱️",
    group: "Marketing and social",
    description:
      "Plans, audits and improves Google and Microsoft search ads, Performance Max and Shopping included: tracking first, then structure, keywords and search terms, ads, bids and budgets, judged by profit, not clicks.",
    starter: false,
    path: "library/athena-agents/marketing/search-ads-strategist.md",
  },
  {
    name: "SEO Specialist",
    emoji: "🌐",
    group: "Marketing and social",
    description:
      "Gets a website found in search: fixes what keeps pages out of Google, matches pages to what people search for, sets up local listings for small businesses, and tracks results in Search Console.",
    starter: false,
    path: "library/athena-agents/marketing/seo-specialist.md",
  },
  {
    name: "Social Media Strategist",
    emoji: "📣",
    group: "Marketing and social",
    description:
      "Plans a brand's or creator's social media across platforms, Instagram in depth: which ones, what to post and how often, the posts and replies themselves, and a monthly read of what's working.",
    starter: false,
    path: "library/athena-agents/marketing/social-media-strategist.md",
  },
  {
    name: "TikTok Strategist",
    emoji: "🎵",
    group: "Marketing and social",
    description:
      "Plans and scripts TikTok videos and photo carousels for people who film on a phone, adapts them for Reels and Shorts, and learns from each batch's results what to make next.",
    starter: false,
    path: "library/athena-agents/marketing/tiktok-strategist.md",
  },
  {
    name: "X Writer",
    emoji: "🐦",
    group: "Marketing and social",
    description:
      "Writes posts, threads and replies for your X accounts in your own voice, reads your mentions for what needs an answer, and leaves every draft for your approval.",
    starter: true,
    path: "library/athena-agents/marketing/x-writer.md",
  },
  {
    name: "YouTube Strategist",
    emoji: "📺",
    group: "Marketing and social",
    description:
      "Helps creators and businesses grow a YouTube channel: what to make, titles and thumbnails, scripts that hold viewers, chapters, descriptions and Shorts, and what YouTube Studio's numbers say to do next.",
    starter: false,
    path: "library/athena-agents/marketing/youtube-strategist.md",
  },
  {
    name: "Deal Strategist",
    emoji: "🤝",
    group: "Sales and support",
    description:
      "Helps you win a specific business deal once you're talking to the buyer: plans and debriefs calls, maps who decides, qualifies honestly, handles objections and competitors, and plans next steps to signature.",
    starter: false,
    path: "library/athena-agents/sales/deal-strategist.md",
  },
  {
    name: "Offer Strategist",
    emoji: "🎁",
    group: "Sales and support",
    description:
      "Designs offers people want to buy and lead magnets that bring the right buyers: the promise, what's included, guarantees and packages; pick it before a launch or when sales stall.",
    starter: false,
    path: "library/athena-agents/sales/offer-strategist.md",
  },
  {
    name: "Outbound Strategist",
    emoji: "🏹",
    group: "Sales and support",
    description:
      "Finds the prospects worth contacting and why now, researches each account, drafts short personal emails, LinkedIn messages and follow-ups, and learns from the replies, for founders and small sales teams.",
    starter: false,
    path: "library/athena-agents/sales/outbound-strategist.md",
  },
  {
    name: "Proposal Writer",
    emoji: "📑",
    group: "Sales and support",
    description:
      "Writes sales proposals, quotes, statements of work and RFP or tender responses that answer exactly what the buyer asked, with a clear plan, honest pricing and proof you can deliver.",
    starter: false,
    path: "library/athena-agents/sales/proposal-writer.md",
  },
  {
    name: "Support Responder",
    emoji: "🎧",
    group: "Sales and support",
    description:
      "Answers your customers: drafts replies to tickets, emails, chats and reviews, handles complaints and refund requests, writes help articles and saved replies, and spots customers who are drifting away.",
    starter: false,
    path: "library/athena-agents/sales/support-responder.md",
  },
  {
    name: "Feedback Synthesizer",
    emoji: "📥",
    group: "Product and projects",
    description:
      "Turns piles of user feedback, from support tickets and reviews to surveys, sales notes and community threads, into a short, honest picture of what people need and what to do next.",
    starter: false,
    path: "library/athena-agents/product/feedback-synthesizer.md",
  },
  {
    name: "Meeting Notes Taker",
    emoji: "🗒️",
    group: "Product and projects",
    description:
      "Turns a meeting's transcript, recording or rough notes into short notes: decisions, who does what by when, open questions, a summary for those who weren't there, and the follow-up email.",
    starter: false,
    path: "library/athena-agents/product/meeting-notes-taker.md",
  },
  {
    name: "Product Manager",
    emoji: "🧭",
    group: "Product and projects",
    description:
      "Decides what to build next and why: finds the real problem, weighs the evidence, sets priorities, writes the spec and how success is measured; pick it when ideas outnumber time.",
    starter: false,
    path: "library/athena-agents/product/product-manager.md",
  },
  {
    name: "Project Manager",
    emoji: "🗓️",
    group: "Product and projects",
    description:
      "Turns a goal into a realistic plan of tasks, owners and dates, tracks progress and risks, keeps everyone informed and flags slips early; pick it to get a team's project delivered.",
    starter: false,
    path: "library/athena-agents/product/project-manager.md",
  },
  {
    name: "Trend Researcher",
    emoji: "🔭",
    group: "Product and projects",
    description:
      "Researches markets, competitors and trends for a product or business decision: who else solves the problem, how they price and position, what's changing, and where the real openings are.",
    starter: false,
    path: "library/athena-agents/product/trend-researcher.md",
  },
  {
    name: "Business Strategist",
    emoji: "♟️",
    group: "Research and analysis",
    description:
      "Helps founders and small businesses make the big calls: where to compete, which customers to serve, the business model, whether to expand, partner, raise money or pivot, and the plan to follow.",
    starter: false,
    path: "library/athena-agents/research/business-strategist.md",
  },
  {
    name: "Data Analyst",
    emoji: "📊",
    group: "Research and analysis",
    description:
      "Answers business questions from spreadsheets, exports and databases: checks the data, finds what moved a number and why, and makes honest charts, dashboards and reports that anyone can act on.",
    starter: false,
    path: "library/athena-agents/research/data-analyst.md",
  },
  {
    name: "Executive Summary Writer",
    emoji: "📌",
    group: "Research and analysis",
    description:
      "Turns long reports, research, data, threads or a project's state into a short summary a busy decision-maker can read in two minutes: what matters, what it means and what to decide.",
    starter: false,
    path: "library/athena-agents/research/executive-summary-writer.md",
  },
  {
    name: "Researcher",
    emoji: "🔎",
    group: "Research and analysis",
    description:
      "Finds and reads the original sources, compares them and answers with links, how sure it is and what it couldn't find, for anything from a quick fact check to a literature review.",
    starter: true,
    path: "library/athena-agents/research/researcher.md",
  },
  {
    name: "Statistician",
    emoji: "🎲",
    group: "Research and analysis",
    description:
      "Designs studies and experiments, sizes them, runs the right analysis and checks other people's statistics in papers, A/B tests and surveys, saying plainly what the numbers can and can't support.",
    starter: false,
    path: "library/athena-agents/research/statistician.md",
  },
  {
    name: "Tool Evaluator",
    emoji: "⚖️",
    group: "Research and analysis",
    description:
      "Helps you choose apps, SaaS tools, libraries, hosting, AI tools or devices: tests the shortlist on your real work, works out the true cost and risks, and recommends one with a runner-up.",
    starter: false,
    path: "library/athena-agents/research/tool-evaluator.md",
  },
  {
    name: "Bookkeeper",
    emoji: "📒",
    group: "Business and finance",
    description:
      "Keeps a small business's or freelancer's books right: categorises transactions, reconciles bank and card accounts, tracks invoices, bills, receipts and VAT or sales tax, closes each month and readies everything for the accountant.",
    starter: false,
    path: "library/athena-agents/business/bookkeeper.md",
  },
  {
    name: "Chief of Staff",
    emoji: "📋",
    group: "Business and finance",
    description:
      "Acts as a founder's or executive's right hand: keeps their priorities straight, prepares meetings and decisions, follows up on every commitment, filters what reaches them and keeps the weekly rhythm running.",
    starter: false,
    path: "library/athena-agents/business/chief-of-staff.md",
  },
  {
    name: "Coordinator",
    emoji: "🗂️",
    group: "Business and finance",
    description:
      "Plans a job, gives each part to the right teammate, checks what comes back and puts it together; pick it for work that needs several specialists.",
    starter: true,
    path: "library/athena-agents/business/coordinator.md",
  },
  {
    name: "Document Maker",
    emoji: "🖨️",
    group: "Business and finance",
    description:
      "Makes finished Word documents, PDFs, Excel workbooks and PowerPoint decks, ready to send or print, from a brief, draft, data or template, and fixes, converts, fills in or merges existing files.",
    starter: false,
    path: "library/athena-agents/business/document-maker.md",
  },
  {
    name: "Financial Analyst",
    emoji: "🧮",
    group: "Business and finance",
    description:
      "Builds budgets, cash forecasts and financial models for small businesses and households, and answers money questions: can we afford it, what if sales drop, is this loan worth it?",
    starter: false,
    path: "library/athena-agents/business/financial-analyst.md",
  },
  {
    name: "Grant Writer",
    emoji: "🌱",
    group: "Business and finance",
    description:
      "Finds grants that fit, says whether each is worth applying for, and writes the application, budget and reports to the funder's own criteria, for nonprofits, researchers, artists, startups and small businesses.",
    starter: false,
    path: "library/athena-agents/business/grant-writer.md",
  },
  {
    name: "Investment Researcher",
    emoji: "💹",
    group: "Business and finance",
    description:
      "Researches shares, funds and private deals for people deciding for themselves: reads the filings, values the business as a range, weighs the bull and bear case, and explains risks and costs plainly.",
    starter: false,
    path: "library/athena-agents/business/investment-researcher.md",
  },
  {
    name: "Legal Document Reviewer",
    emoji: "📜",
    group: "Business and finance",
    description:
      "Reads a contract, lease, NDA or set of terms before you sign, explains in plain words what's risky, unusual or missing, drafts the changes to ask for, and checks your own documents.",
    starter: false,
    path: "library/athena-agents/business/legal-document-reviewer.md",
  },
  {
    name: "Mail Assistant",
    emoji: "✉️",
    group: "Business and finance",
    description:
      "Reads and sorts your Gmail, says what needs you today, drafts replies in your voice, tracks follow-ups and finds meeting times, through the Gmail and Calendar connectors on your Claude account.",
    starter: true,
    path: "library/athena-agents/business/mail-assistant.md",
  },
  {
    name: "Notes Steward",
    emoji: "📇",
    group: "Business and finance",
    description:
      "Turns reading, meetings and ideas into short linked notes in your words, keeps an Obsidian vault, Markdown folder or the project's notes tidy, and answers questions from them with links.",
    starter: false,
    path: "library/athena-agents/business/notes-steward.md",
  },
  {
    name: "Operations Manager",
    emoji: "🛠️",
    group: "Business and finance",
    description:
      "Maps how work really gets done, removes bottlenecks and errors, automates what's left and writes the procedures people follow; pick it when daily work is slow, error-prone or depends on one person.",
    starter: false,
    path: "library/athena-agents/business/operations-manager.md",
  },
  {
    name: "Pricing Analyst",
    emoji: "💲",
    group: "Business and finance",
    description:
      "Works out what to charge and how: price levels, per-seat, usage or tiered plans, discounts, promotions and price rises, from real margins and buyer behaviour; pick it before setting or changing a price.",
    starter: false,
    path: "library/athena-agents/business/pricing-analyst.md",
  },
  {
    name: "Resume Tailor",
    emoji: "👔",
    group: "Business and finance",
    description:
      "Tailors your CV or resume and cover letter to one job, from your real experience, so recruiters and hiring software see the fit, and gets you ready for the interview.",
    starter: false,
    path: "library/athena-agents/business/resume-tailor.md",
  },
  {
    name: "Translator",
    emoji: "🗣️",
    group: "Business and finance",
    description:
      "Translates and localises between any two languages: documents, websites and app strings, marketing, emails, subtitles and everyday messages, keeping meaning, tone and formatting, and adapting what the reader's country needs.",
    starter: false,
    path: "library/athena-agents/business/translator.md",
  },
];
