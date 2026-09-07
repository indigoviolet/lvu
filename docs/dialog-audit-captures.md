# Dialog audit captures

Raw `TestBackend` captures of `ui::render` on `main` @ 1d0c44f, used as the
evidence for the audit section of `dialog-design.md`. Findings live there; this
file is the unedited output.

Each block header records the terminal size, the focused surface,
`hit_regions.selection_modal` (the rect shared by rendering, mouse hitboxes and
text selection), the dialog and help scroll limits, and the mouse scroll hitbox.

Dialog state is populated, not empty: a nine-entry storage snapshot, six saved
recipes, an accepted Source 🧠 proposal, an accepted Ask 🧠 proposal, a bookmark
note draft, a filled External command definition and a six-message
investigation transcript.

```text

===== main-no-dialog @ 140x40 focus=Logs surface=None dialog_scroll=0 help_scroll=0 scroll_hitbox=None =====
lvu log workspace DEMO FIXTURE — NOT ACQUISITION                                                                                            
┌ Sources / views ───┐┌ Log viewport ──────────────────────────────────────────────────────────────────────────────────────────────────────┐
│● API fixture       ││time          level  event                                                                                          │
│  synthetic/live    ││12:00:01      INFO   fixture request 01 completed                                                                   │
│ › All events       ││12:00:02      INFO   fixture request 02 completed                                                                   │
│● Worker fixture    ││12:00:03      INFO   Unicode 東 京  café é request completed                                                          │
│  synthetic/static  ││12:00:04      INFO   fixture request 04 completed                                                                   │
│   Errors only      ││12:00:05      WARN   fixture request 05 completed                                                                   │
│                    ││12:00:06      INFO   fixture request 06 completed                                                                   │
│                    ││12:00:07      INFO   fixture request 07 completed                                                                   │
│                    ││12:00:08      INFO   fixture request 08 completed                                                                   │
│                    ││12:00:09      INFO   fixture request 09 completed                                                                   │
│                    ││12:00:10      WARN   fixture request 10 completed                                                                   │
│                    ││12:00:11      INFO   fixture request 11 completed                                                                   │
│                    ││12:00:12      INFO   fixture request 12 completed                                                                   │
│                    ││12:00:13      INFO   fixture request 13 completed                                                                   │
│                    ││12:00:14      INFO   fixture request 14 completed                                                                   │
│                    ││12:00:15      WARN   fixture request 15 completed                                                                   │
│                    ││12:00:16      INFO   fixture request 16 completed                                                                   │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
└────────────────────┘└────────────────────────────────────────────────────────────────────────────────────────────────────────────────────┘
                       FOLLOW | 1-16/16 | ? help                                                                                            

===== main-no-dialog @ 100x30 focus=Logs surface=None dialog_scroll=0 help_scroll=0 scroll_hitbox=None =====
lvu log workspace DEMO FIXTURE — NOT ACQUISITION                                                    
┌ Sources / views ───┐┌ Log viewport ──────────────────────────────────────────────────────────────┐
│● API fixture       ││time          level  event                                                  │
│  synthetic/live    ││12:00:01      INFO   fixture request 01 completed                           │
│ › All events       ││12:00:02      INFO   fixture request 02 completed                           │
│● Worker fixture    ││12:00:03      INFO   Unicode 東 京  café é request completed                  │
│  synthetic/static  ││12:00:04      INFO   fixture request 04 completed                           │
│   Errors only      ││12:00:05      WARN   fixture request 05 completed                           │
│                    ││12:00:06      INFO   fixture request 06 completed                           │
│                    ││12:00:07      INFO   fixture request 07 completed                           │
│                    ││12:00:08      INFO   fixture request 08 completed                           │
│                    ││12:00:09      INFO   fixture request 09 completed                           │
│                    ││12:00:10      WARN   fixture request 10 completed                           │
│                    ││12:00:11      INFO   fixture request 11 completed                           │
│                    ││12:00:12      INFO   fixture request 12 completed                           │
│                    ││12:00:13      INFO   fixture request 13 completed                           │
│                    ││12:00:14      INFO   fixture request 14 completed                           │
│                    ││12:00:15      WARN   fixture request 15 completed                           │
│                    ││12:00:16      INFO   fixture request 16 completed                           │
│                    ││                                                                            │
│                    ││                                                                            │
│                    ││                                                                            │
│                    ││                                                                            │
│                    ││                                                                            │
│                    ││                                                                            │
│                    ││                                                                            │
│                    ││                                                                            │
│                    ││                                                                            │
└────────────────────┘└────────────────────────────────────────────────────────────────────────────┘
                       FOLLOW | 1-16/16 | ? help                                                    

===== main-no-dialog @ 80x24 focus=Logs surface=None dialog_scroll=0 help_scroll=0 scroll_hitbox=None =====
lvu log workspace DEMO FIXTURE — NOT ACQUISITION                                
┌ Sources / views ───┐┌ Log viewport ──────────────────────────────────────────┐
│● API fixture       ││time          level  event                              │
│  synthetic/live    ││12:00:01      INFO   fixture request 01 completed       │
│ › All events       ││12:00:02      INFO   fixture request 02 completed       │
│● Worker fixture    ││12:00:03      INFO   Unicode 東 京  café é request complet│
│  synthetic/static  ││12:00:04      INFO   fixture request 04 completed       │
│   Errors only      ││12:00:05      WARN   fixture request 05 completed       │
│                    ││12:00:06      INFO   fixture request 06 completed       │
│                    ││12:00:07      INFO   fixture request 07 completed       │
│                    ││12:00:08      INFO   fixture request 08 completed       │
│                    ││12:00:09      INFO   fixture request 09 completed       │
│                    ││12:00:10      WARN   fixture request 10 completed       │
│                    ││12:00:11      INFO   fixture request 11 completed       │
│                    ││12:00:12      INFO   fixture request 12 completed       │
│                    ││12:00:13      INFO   fixture request 13 completed       │
│                    ││12:00:14      INFO   fixture request 14 completed       │
│                    ││12:00:15      WARN   fixture request 15 completed       │
│                    ││12:00:16      INFO   fixture request 16 completed       │
│                    ││                                                        │
│                    ││                                                        │
│                    ││                                                        │
└────────────────────┘└────────────────────────────────────────────────────────┘
                       FOLLOW | 1-16/16 | ? help                                

===== main-no-dialog @ 54x16 focus=Logs surface=None dialog_scroll=0 help_scroll=0 scroll_hitbox=None =====
lvu log workspace DEMO FIXTURE — NOT ACQUISITION      
┌ Sources / views ───┐┌ Log viewport ────────────────┐
│● API fixture       ││time          level  event    │
│  synthetic/live    ││12:00:06      INFO   fixture r│
│ › All events       ││12:00:07      INFO   fixture r│
│● Worker fixture    ││12:00:08      INFO   fixture r│
│  synthetic/static  ││12:00:09      INFO   fixture r│
│   Errors only      ││12:00:10      WARN   fixture r│
│                    ││12:00:11      INFO   fixture r│
│                    ││12:00:12      INFO   fixture r│
│                    ││12:00:13      INFO   fixture r│
│                    ││12:00:14      INFO   fixture r│
│                    ││12:00:15      WARN   fixture r│
│                    ││12:00:16      INFO   fixture r│
└────────────────────┘└──────────────────────────────┘
                       FOLLOW | 6-16/16 | ? help      

===== help @ 140x40 focus=Help surface=Some(Rect { x: 5, y: 6, width: 129, height: 28 }) dialog_scroll=0 help_scroll=2 scroll_hitbox=None =====
lvu log workspace DEMO FIXTURE — NOT ACQUISITION                                                                                            
┌ Sources / views ───┐┌ Log viewport ──────────────────────────────────────────────────────────────────────────────────────────────────────┐
│● API fixture       ││time          level  event                                                                                          │
│  synthetic/live    ││12:00:01      INFO   fixture request 01 completed                                                                   │
│ › All events       ││12:00:02      INFO   fixture request 02 completed                                                                   │
│● W┌ Help ───────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────┐    │
│  s│ EVERYWHERE                                                      SOURCES                                                         │    │
│   │   Ctrl-P               Open the command palette                   n              Add a source                                   │    │
│   │   ?                    Open or close this help                    Alt-F / Alt-C  Choose file / command in Add source            │    │
│   │   Ctrl-L               Redraw the terminal                        Ctrl-D         Discover sources; selection never auto-starts  │    │
│   │   ,                    Open settings                              Ctrl-A         Ask 🧠  to draft a source for review            │    │
│   │   q / Ctrl-C           Quit                                       v              Open view actions                              │    │
│   │                                                                   Alt-M          Edit source membership in View actions         │    │
│   │ LOGS & VIEWS                                                                                                                    │    │
│   │   g / G                Jump to first / last record              VIEWS & RECIPES                                                 │    │
│   │   ←/→ · 0              Pan the selected event / reset pan         Alt-B          Create a blank view                            │    │
│   │   [ / ]                Previous or next view                      Alt-D          Clone the current view                         │    │
│   │   f                    Toggle follow / history                    Alt-R          Rename the current view                        │    │
│   │   d                    Toggle selected-record details             r              Browse named recipes                           │    │
│   │   o                    Open raw context                                                                                         │    │
│   │   b                    Toggle a bookmark                        ASSISTANCE                                                      │    │
│   │   B                    Open bookmarks and notes                   A              Ask 🧠  for a filter or enrichment proposal     │    │
│   │   Alt-S                Stop the selected source                   I              Open a local 🧠  investigation                  │    │
│   │   Alt-R                Restart the selected source                Alt-N          Start a new investigation snapshot             │    │
│   │                                                                                                                                 │    │
│   │ FILTER & SHAPE                                                                                                                  │    │
│   │   /                    Literal or field-aware search                                                                            │    │
│   │   p                    Open the advanced filter                                                                                 │    │
│   │   e                    Open ordered enrichments                                                                                 │    │
│   │   Alt-C in Enrichment  Add, edit, remove, or explicitly run                                                                     │    │
│   │ the terminal command step                                                                                                       │    │
│   │   m                    Open display-only grouping                                                                               │    │
│   │   i                    Inspect fields; Space pins, c colors                                                                     │    │
│   │↑/↓ or j/k scroll · ? close                                                                                                      │    │
│   └─────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────┘    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
└────────────────────┘└────────────────────────────────────────────────────────────────────────────────────────────────────────────────────┘
                       FOLLOW | 1-16/16 | ? help                                                                                            

===== help @ 100x30 focus=Help surface=Some(Rect { x: 4, y: 2, width: 92, height: 26 }) dialog_scroll=0 help_scroll=22 scroll_hitbox=None =====
lvu log workspace DEMO FIXTURE — NOT ACQUISITION                                                    
┌ S┌ Help ──────────────────────────────────────────────────────────────────────────────────────┐──┐
│● │ EVERYWHERE                                                                                 │  │
│  │   Ctrl-P               Open the command palette                                            │  │
│ ›│   ?                    Open or close this help                                             │  │
│● │   Ctrl-L               Redraw the terminal                                                 │  │
│  │   ,                    Open settings                                                       │  │
│  │   q / Ctrl-C           Quit                                                                │  │
│  │                                                                                            │  │
│  │ LOGS & VIEWS                                                                               │  │
│  │   g / G                Jump to first / last record                                         │  │
│  │   ←/→ · 0              Pan the selected event / reset pan                                  │  │
│  │   [ / ]                Previous or next view                                               │  │
│  │   f                    Toggle follow / history                                             │  │
│  │   d                    Toggle selected-record details                                      │  │
│  │   o                    Open raw context                                                    │  │
│  │   b                    Toggle a bookmark                                                   │  │
│  │   B                    Open bookmarks and notes                                            │  │
│  │   Alt-S                Stop the selected source                                            │  │
│  │   Alt-R                Restart the selected source                                         │  │
│  │                                                                                            │  │
│  │ FILTER & SHAPE                                                                             │  │
│  │   /                    Literal or field-aware search                                       │  │
│  │   p                    Open the advanced filter                                            │  │
│  │   e                    Open ordered enrichments                                            │  │
│  │   Alt-C in Enrichment  Add, edit, remove, or explicitly run the terminal command step      │  │
│  │   m                    Open display-only grouping                                          │  │
│  │↑/↓ or j/k scroll · ? close                                                                 │  │
└──└────────────────────────────────────────────────────────────────────────────────────────────┘──┘
                       FOLLOW | 1-16/16 | ? help                                                    

===== help @ 80x24 focus=Help surface=Some(Rect { x: 3, y: 2, width: 73, height: 20 }) dialog_scroll=0 help_scroll=29 scroll_hitbox=None =====
lvu log workspace DEMO FIXTURE — NOT ACQUISITION                                
┌ ┌ Help ───────────────────────────────────────────────────────────────────┐──┐
│●│ EVERYWHERE                                                              │  │
│ │   Ctrl-P               Open the command palette                         │  │
│ │   ?                    Open or close this help                          │  │
│●│   Ctrl-L               Redraw the terminal                              │et│
│ │   ,                    Open settings                                    │  │
│ │   q / Ctrl-C           Quit                                             │  │
│ │                                                                         │  │
│ │ LOGS & VIEWS                                                            │  │
│ │   g / G                Jump to first / last record                      │  │
│ │   ←/→ · 0              Pan the selected event / reset pan               │  │
│ │   [ / ]                Previous or next view                            │  │
│ │   f                    Toggle follow / history                          │  │
│ │   d                    Toggle selected-record details                   │  │
│ │   o                    Open raw context                                 │  │
│ │   b                    Toggle a bookmark                                │  │
│ │   B                    Open bookmarks and notes                         │  │
│ │   Alt-S                Stop the selected source                         │  │
│ │   Alt-R                Restart the selected source                      │  │
│ │                                                                         │  │
│ │↑/↓ or j/k scroll · ? close                                              │  │
└─└─────────────────────────────────────────────────────────────────────────┘──┘
                       FOLLOW | 1-16/16 | ? help                                

===== help @ 54x16 focus=Help surface=Some(Rect { x: 3, y: 2, width: 48, height: 12 }) dialog_scroll=0 help_scroll=58 scroll_hitbox=None =====
lvu log workspace DEMO FIXTURE — NOT ACQUISITION      
┌ ┌ Help ──────────────────────────────────────────┐─┐
│●│ EVERYWHERE                                     │ │
│ │   Ctrl-P               Open the command        │r│
│ │ palette                                        │r│
│●│   ?                    Open or close this help │r│
│ │   Ctrl-L               Redraw the terminal     │r│
│ │   ,                    Open settings           │r│
│ │   q / Ctrl-C           Quit                    │r│
│ │                                                │r│
│ │ LOGS & VIEWS                                   │r│
│ │   g / G                Jump to first / last    │r│
│ │ record                                         │r│
│ │↑/↓ or j/k scroll · ? close                     │r│
└─└────────────────────────────────────────────────┘─┘
                       FOLLOW | 6-16/16 | ? help      

===== context-raw @ 140x40 focus=Context surface=Some(Rect { x: 1, y: 8, width: 138, height: 24 }) dialog_scroll=0 help_scroll=0 scroll_hitbox=None =====
lvu log workspace DEMO FIXTURE — NOT ACQUISITION                                                                                            
┌ Sources / views ───┐                                                                                                                      
│● API fixture       │                                                                                                                      
│  synthetic/live    │                                                                                                                      
│ › All events       │                                                                                                                      
│● Worker fixture    │                                                                                                                      
│  synthetic/static  │                                                                                                                      
┌ Raw context · filter unchanged ──────────────────────────────────────────────────────────────────────────────────────────────────────────┐
│ Anchor: api:16 · physical source records                                                                                                 │
│ 11–16 / 16 · raw, unfiltered, ungrouped                                                                                                  │
│       11 fixture request 11 completed                                                                                                    │
│       12 fixture request 12 completed                                                                                                    │
│       13 fixture request 13 completed                                                                                                    │
│       14 fixture request 14 completed                                                                                                    │
│       15 fixture request 15 completed                                                                                                    │
│ >     16 fixture request 16 completed                                                                                                    │
│                                                                                                                                          │
│                                                                                                                                          │
│                                                                                                                                          │
│                                                                                                                                          │
│                                                                                                                                          │
│                                                                                                                                          │
│                                                                                                                                          │
│                                                                                                                                          │
│                                                                                                                                          │
│                                                                                                                                          │
│                                                                                                                                          │
│                                                                                                                                          │
│                                                                                                                                          │
│                                                                                                                                          │
│                                                                                                                                          │
│↑/↓ scroll · g anchor                                                                                                                     │
└──────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────┘
│                    │                                                                                                                      
│                    │                                                                                                                      
│                    │                                                                                                                      
│                    │                                                                                                                      
│                    │                                                                                                                      
└────────────────────┘                                                                                                                      
                       FOLLOW | 1-16/16 | ? help                                                                                            

===== context-raw @ 100x30 focus=Context surface=Some(Rect { x: 1, y: 3, width: 98, height: 24 }) dialog_scroll=0 help_scroll=0 scroll_hitbox=None =====
lvu log workspace DEMO FIXTURE — NOT ACQUISITION                                                    
┌ Sources / views ───┐                                                                              
┌ Raw context · filter unchanged ──────────────────────────────────────────────────────────────────┐
│ Anchor: api:16 · physical source records                                                         │
│ 11–16 / 16 · raw, unfiltered, ungrouped                                                          │
│       11 fixture request 11 completed                                                            │
│       12 fixture request 12 completed                                                            │
│       13 fixture request 13 completed                                                            │
│       14 fixture request 14 completed                                                            │
│       15 fixture request 15 completed                                                            │
│ >     16 fixture request 16 completed                                                            │
│                                                                                                  │
│                                                                                                  │
│                                                                                                  │
│                                                                                                  │
│                                                                                                  │
│                                                                                                  │
│                                                                                                  │
│                                                                                                  │
│                                                                                                  │
│                                                                                                  │
│                                                                                                  │
│                                                                                                  │
│                                                                                                  │
│                                                                                                  │
│                                                                                                  │
│↑/↓ scroll · g anchor                                                                             │
└──────────────────────────────────────────────────────────────────────────────────────────────────┘
└────────────────────┘                                                                              
                       FOLLOW | 1-16/16 | ? help                                                    

===== context-raw @ 80x24 focus=Context surface=Some(Rect { x: 1, y: 1, width: 78, height: 22 }) dialog_scroll=0 help_scroll=0 scroll_hitbox=None =====
┌ Raw context · filter unchanged ──────────────────────────────────────────────┐
│ Anchor: api:16 · physical source records                                     │
│ 11–16 / 16 · raw, unfiltered, ungrouped                                      │
│       11 fixture request 11 completed                                        │
│       12 fixture request 12 completed                                        │
│       13 fixture request 13 completed                                        │
│       14 fixture request 14 completed                                        │
│       15 fixture request 15 completed                                        │
│ >     16 fixture request 16 completed                                        │
│                                                                              │
│                                                                              │
│                                                                              │
│                                                                              │
│                                                                              │
│                                                                              │
│                                                                              │
│                                                                              │
│                                                                              │
│                                                                              │
│                                                                              │
│                                                                              │
│                                                                              │
│↑/↓ scroll · g anchor                                                         │
└──────────────────────────────────────────────────────────────────────────────┘

===== context-raw @ 54x16 focus=Context surface=Some(Rect { x: 1, y: 1, width: 52, height: 14 }) dialog_scroll=0 help_scroll=0 scroll_hitbox=None =====
┌ Raw context · filter unchanged ────────────────────┐
│ Anchor: api:16 · physical source records           │
│ 11–16 / 16 · raw, unfiltered, ungrouped            │
│       11 fixture request 11 completed              │
│       12 fixture request 12 completed              │
│       13 fixture request 13 completed              │
│       14 fixture request 14 completed              │
│       15 fixture request 15 completed              │
│ >     16 fixture request 16 completed              │
│                                                    │
│                                                    │
│                                                    │
│                                                    │
│                                                    │
│↑/↓ scroll · g anchor                               │
└────────────────────────────────────────────────────┘

===== details-pane @ 140x40 focus=Details surface=None dialog_scroll=0 help_scroll=0 scroll_hitbox=None =====
lvu log workspace DEMO FIXTURE — NOT ACQUISITION                                                                                            
┌ Sources / views ───┐┌ Log viewport ──────────────────────────────────────────────────────────────────────────────────────────────────────┐
│● API fixture       ││time          level  event                                                                                          │
│  synthetic/live    ││12:00:01      INFO   fixture request 01 completed                                                                   │
│ › All events       ││12:00:02      INFO   fixture request 02 completed                                                                   │
│● Worker fixture    ││12:00:03      INFO   Unicode 東 京  café é request completed                                                          │
│  synthetic/static  ││12:00:04      INFO   fixture request 04 completed                                                                   │
│   Errors only      ││12:00:05      WARN   fixture request 05 completed                                                                   │
│                    ││12:00:06      INFO   fixture request 06 completed                                                                   │
│                    ││12:00:07      INFO   fixture request 07 completed                                                                   │
│                    ││12:00:08      INFO   fixture request 08 completed                                                                   │
│                    ││12:00:09      INFO   fixture request 09 completed                                                                   │
│                    ││12:00:10      WARN   fixture request 10 completed                                                                   │
│                    ││12:00:11      INFO   fixture request 11 completed                                                                   │
│                    ││12:00:12      INFO   fixture request 12 completed                                                                   │
│                    ││12:00:13      INFO   fixture request 13 completed                                                                   │
│                    ││12:00:14      INFO   fixture request 14 completed                                                                   │
│                    ││12:00:15      WARN   fixture request 15 completed                                                                   │
│                    ││12:00:16      INFO   fixture request 16 completed                                                                   │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
│                    │└────────────────────────────────────────────────────────────────────────────────────────────────────────────────────┘
│                    │┌ Selected event details ────────────────────────────────────────────────────────────────────────────────────────────┐
│                    ││stable display id: api:16                                                                                           │
│                    ││raw: fixture request 16 completed                                                                                   │
│                    ││service: api                                                                                                        │
│                    ││level: INFO                                                                                                         │
│                    ││fixture: true (not captured data)                                                                                   │
│                    ││record_id: api:16                                                                                                   │
│                    ││message: fixture request 16 completed                                                                               │
│                    ││event_time_utc_nanos: 116000000000                                                                                  │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
│                    ││↑/↓ scroll                                                                                                          │
└────────────────────┘└────────────────────────────────────────────────────────────────────────────────────────────────────────────────────┘
                       FOLLOW | 1-16/16 | ? help                                                                                            

===== details-pane @ 100x30 focus=Details surface=None dialog_scroll=0 help_scroll=0 scroll_hitbox=None =====
lvu log workspace DEMO FIXTURE — NOT ACQUISITION                                                    
┌ Sources / views ───┐┌ Log viewport ──────────────────────────────────────────────────────────────┐
│● API fixture       ││time          level  event                                                  │
│  synthetic/live    ││12:00:02      INFO   fixture request 02 completed                           │
│ › All events       ││12:00:03      INFO   Unicode 東 京  café é request completed                  │
│● Worker fixture    ││12:00:04      INFO   fixture request 04 completed                           │
│  synthetic/static  ││12:00:05      WARN   fixture request 05 completed                           │
│   Errors only      ││12:00:06      INFO   fixture request 06 completed                           │
│                    ││12:00:07      INFO   fixture request 07 completed                           │
│                    ││12:00:08      INFO   fixture request 08 completed                           │
│                    ││12:00:09      INFO   fixture request 09 completed                           │
│                    ││12:00:10      WARN   fixture request 10 completed                           │
│                    ││12:00:11      INFO   fixture request 11 completed                           │
│                    ││12:00:12      INFO   fixture request 12 completed                           │
│                    ││12:00:13      INFO   fixture request 13 completed                           │
│                    ││12:00:14      INFO   fixture request 14 completed                           │
│                    ││12:00:15      WARN   fixture request 15 completed                           │
│                    ││12:00:16      INFO   fixture request 16 completed                           │
│                    │└────────────────────────────────────────────────────────────────────────────┘
│                    │┌ Selected event details ────────────────────────────────────────────────────┐
│                    ││stable display id: api:16                                                   │
│                    ││raw: fixture request 16 completed                                           │
│                    ││service: api                                                                │
│                    ││level: INFO                                                                 │
│                    ││fixture: true (not captured data)                                           │
│                    ││record_id: api:16                                                           │
│                    ││message: fixture request 16 completed                                       │
│                    ││↑/↓ scroll                                                                  │
└────────────────────┘└────────────────────────────────────────────────────────────────────────────┘
                       FOLLOW | 2-16/16 | ? help                                                    

===== details-pane @ 80x24 focus=Details surface=None dialog_scroll=0 help_scroll=0 scroll_hitbox=None =====
lvu log workspace DEMO FIXTURE — NOT ACQUISITION                                
┌ Sources / views ───┐┌ Log viewport ──────────────────────────────────────────┐
│● API fixture       ││time          level  event                              │
│  synthetic/live    ││12:00:06      INFO   fixture request 06 completed       │
│ › All events       ││12:00:07      INFO   fixture request 07 completed       │
│● Worker fixture    ││12:00:08      INFO   fixture request 08 completed       │
│  synthetic/static  ││12:00:09      INFO   fixture request 09 completed       │
│   Errors only      ││12:00:10      WARN   fixture request 10 completed       │
│                    ││12:00:11      INFO   fixture request 11 completed       │
│                    ││12:00:12      INFO   fixture request 12 completed       │
│                    ││12:00:13      INFO   fixture request 13 completed       │
│                    ││12:00:14      INFO   fixture request 14 completed       │
│                    ││12:00:15      WARN   fixture request 15 completed       │
│                    ││12:00:16      INFO   fixture request 16 completed       │
│                    │└────────────────────────────────────────────────────────┘
│                    │┌ Selected event details ────────────────────────────────┐
│                    ││stable display id: api:16                               │
│                    ││raw: fixture request 16 completed                       │
│                    ││service: api                                            │
│                    ││level: INFO                                             │
│                    ││fixture: true (not captured data)                       │
│                    ││↑/↓ scroll                                              │
└────────────────────┘└────────────────────────────────────────────────────────┘
                       FOLLOW | 6-16/16 | ? help                                

===== details-pane @ 54x16 focus=Details surface=None dialog_scroll=0 help_scroll=0 scroll_hitbox=None =====
lvu log workspace DEMO FIXTURE — NOT ACQUISITION      
┌ Sources / views ───┐┌ Log viewport ────────────────┐
│● API fixture       ││time          level  event    │
│  synthetic/live    ││12:00:11      INFO   fixture r│
│ › All events       ││12:00:12      INFO   fixture r│
│● Worker fixture    ││12:00:13      INFO   fixture r│
│  synthetic/static  ││12:00:14      INFO   fixture r│
│   Errors only      ││12:00:15      WARN   fixture r│
│                    ││12:00:16      INFO   fixture r│
│                    │└──────────────────────────────┘
│                    │┌ Selected event details ──────┐
│                    ││stable display id: api:16     │
│                    ││raw: fixture request 16       │
│                    ││↑/↓ scroll                    │
└────────────────────┘└──────────────────────────────┘
                       FOLLOW | 11-16/16 | ? help     

===== fields @ 140x40 focus=FieldPicker surface=Some(Rect { x: 22, y: 13, width: 96, height: 14 }) dialog_scroll=0 help_scroll=0 scroll_hitbox=None =====
lvu log workspace DEMO FIXTURE — NOT ACQUISITION                                                                                            
┌ Sources / views ───┐┌ Log viewport ──────────────────────────────────────────────────────────────────────────────────────────────────────┐
│● API fixture       ││time          level  event                                                                                          │
│  synthetic/live    ││12:00:01      INFO   fixture request 01 completed                                                                   │
│ › All events       ││12:00:02      INFO   fixture request 02 completed                                                                   │
│● Worker fixture    ││12:00:03      INFO   Unicode 東 京  café é request completed                                                          │
│  synthetic/static  ││12:00:04      INFO   fixture request 04 completed                                                                   │
│   Errors only      ││12:00:05      WARN   fixture request 05 completed                                                                   │
│                    ││12:00:06      INFO   fixture request 06 completed                                                                   │
│                    ││12:00:07      INFO   fixture request 07 completed                                                                   │
│                    ││12:00:08      INFO   fixture request 08 completed                                                                   │
│                    ││12:00:09      INFO   fixture request 09 completed                                                                   │
│                    ┌ Event fields ──────────────────────────────────────────────────────────────────────────────────┐                    │
│                    │ > [ ] service = api                                                                            │                    │
│                    │   [ ] level = INFO                                                                             │                    │
│                    │                                                                                                │                    │
│                    │                                                                                                │                    │
│                    │                                                                                                │                    │
│                    │                                                                                                │                    │
│                    │                                                                                                │                    │
│                    │                                                                                                │                    │
│                    │                                                                                                │                    │
│                    │                                                                                                │                    │
│                    │                                                                                                │                    │
│                    │                                                                                                │                    │
│                    │                                                                                                │                    │
│                    │↑/↓ select · Space pin · c Color rows by this field                                             │                    │
│                    └────────────────────────────────────────────────────────────────────────────────────────────────┘                    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
└────────────────────┘└────────────────────────────────────────────────────────────────────────────────────────────────────────────────────┘
                       FOLLOW | 1-16/16 | ? help                                                                                            

===== fields @ 100x30 focus=FieldPicker surface=Some(Rect { x: 16, y: 8, width: 68, height: 14 }) dialog_scroll=0 help_scroll=0 scroll_hitbox=None =====
lvu log workspace DEMO FIXTURE — NOT ACQUISITION                                                    
┌ Sources / views ───┐┌ Log viewport ──────────────────────────────────────────────────────────────┐
│● API fixture       ││time          level  event                                                  │
│  synthetic/live    ││12:00:01      INFO   fixture request 01 completed                           │
│ › All events       ││12:00:02      INFO   fixture request 02 completed                           │
│● Worker fixture    ││12:00:03      INFO   Unicode 東 京  café é request completed                  │
│  synthetic/static  ││12:00:04      INFO   fixture request 04 completed                           │
│   Errors only┌ Event fields ──────────────────────────────────────────────────────┐              │
│              │ > [ ] service = api                                                │              │
│              │   [ ] level = INFO                                                 │              │
│              │                                                                    │              │
│              │                                                                    │              │
│              │                                                                    │              │
│              │                                                                    │              │
│              │                                                                    │              │
│              │                                                                    │              │
│              │                                                                    │              │
│              │                                                                    │              │
│              │                                                                    │              │
│              │                                                                    │              │
│              │                                                                    │              │
│              │↑/↓ select · Space pin · c Color rows by this field                 │              │
│              └────────────────────────────────────────────────────────────────────┘              │
│                    ││                                                                            │
│                    ││                                                                            │
│                    ││                                                                            │
│                    ││                                                                            │
│                    ││                                                                            │
└────────────────────┘└────────────────────────────────────────────────────────────────────────────┘
                       FOLLOW | 1-16/16 | ? help                                                    

===== fields @ 80x24 focus=FieldPicker surface=Some(Rect { x: 13, y: 5, width: 54, height: 14 }) dialog_scroll=0 help_scroll=0 scroll_hitbox=None =====
lvu log workspace DEMO FIXTURE — NOT ACQUISITION                                
┌ Sources / views ───┐┌ Log viewport ──────────────────────────────────────────┐
│● API fixture       ││time          level  event                              │
│  synthetic/live    ││12:00:01      INFO   fixture request 01 completed       │
│ › All even┌ Event fields ────────────────────────────────────────┐eted       │
│● Worker fi│ > [ ] service = api                                  │est complet│
│  synthetic│   [ ] level = INFO                                   │eted       │
│   Errors o│                                                      │eted       │
│           │                                                      │eted       │
│           │                                                      │eted       │
│           │                                                      │eted       │
│           │                                                      │eted       │
│           │                                                      │eted       │
│           │                                                      │eted       │
│           │                                                      │eted       │
│           │                                                      │eted       │
│           │                                                      │eted       │
│           │                                                      │eted       │
│           │↑/↓ select · Space pin · c Color rows by this field   │eted       │
│           └──────────────────────────────────────────────────────┘           │
│                    ││                                                        │
│                    ││                                                        │
└────────────────────┘└────────────────────────────────────────────────────────┘
                       FOLLOW | 1-16/16 | ? help                                

===== fields @ 54x16 focus=FieldPicker surface=Some(Rect { x: 9, y: 1, width: 35, height: 14 }) dialog_scroll=0 help_scroll=0 scroll_hitbox=None =====
lvu log ┌ Event fields ─────────────────────┐ION      
┌ Source│ > [ ] service = api               │────────┐
│● API f│   [ ] level = INFO                │vent    │
│  synth│                                   │ixture r│
│ › All │                                   │ixture r│
│● Worke│                                   │ixture r│
│  synth│                                   │ixture r│
│   Erro│                                   │ixture r│
│       │                                   │ixture r│
│       │                                   │ixture r│
│       │                                   │ixture r│
│       │                                   │ixture r│
│       │                                   │ixture r│
│       │↑/↓ select · Space pin · c Color   │ixture r│
└───────│rows by this field                 │────────┘
        └───────────────────────────────────┘elp      

===== search @ 140x40 focus=SearchEditor surface=Some(Rect { x: 15, y: 15, width: 110, height: 9 }) dialog_scroll=0 help_scroll=0 scroll_hitbox=None =====
lvu log workspace DEMO FIXTURE — NOT ACQUISITION                                                                                            
┌ Sources / views ───┐┌ Log viewport ──────────────────────────────────────────────────────────────────────────────────────────────────────┐
│● API fixture       ││time          level  event                                                                                          │
│  synthetic/live    ││12:00:01      INFO   fixture request 01 completed                                                                   │
│ › All events       ││12:00:02      INFO   fixture request 02 completed                                                                   │
│● Worker fixture    ││12:00:03      INFO   Unicode 東 京  café é request completed                                                          │
│  synthetic/static  ││12:00:04      INFO   fixture request 04 completed                                                                   │
│   Errors only      ││12:00:05      WARN   fixture request 05 completed                                                                   │
│                    ││12:00:06      INFO   fixture request 06 completed                                                                   │
│                    ││12:00:07      INFO   fixture request 07 completed                                                                   │
│                    ││12:00:08      INFO   fixture request 08 completed                                                                   │
│                    ││12:00:09      INFO   fixture request 09 completed                                                                   │
│                    ││12:00:10      WARN   fixture request 10 completed                                                                   │
│                    ││12:00:11      INFO   fixture request 11 completed                                                                   │
│             ┌ Search ──────────────────────────────────────────────────────────────────────────────────────────────────────┐             │
│             │ level: WARN                                                                                                  │             │
│             │ Applied  No filter applied.                                                                                  │             │
│             │                                                                                                              │             │
│             │                                                                                                              │             │
│             │                                                                                                              │             │
│             │                                                                                                              │             │
│             │ Examples: text · "field name": text · /regex/ims · \/literal                                                 │             │
│             │                                                                                                              │             │
│             │                                                                                                              │             │
│             └──────────────────────────────────────────────────────────────────────────────────────────────────────────────┘             │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
└────────────────────┘└────────────────────────────────────────────────────────────────────────────────────────────────────────────────────┘
                       FOLLOW | 1-16/16 | ? help                                                                                            

===== search @ 100x30 focus=SearchEditor surface=Some(Rect { x: 11, y: 10, width: 78, height: 9 }) dialog_scroll=0 help_scroll=0 scroll_hitbox=None =====
lvu log workspace DEMO FIXTURE — NOT ACQUISITION                                                    
┌ Sources / views ───┐┌ Log viewport ──────────────────────────────────────────────────────────────┐
│● API fixture       ││time          level  event                                                  │
│  synthetic/live    ││12:00:01      INFO   fixture request 01 completed                           │
│ › All events       ││12:00:02      INFO   fixture request 02 completed                           │
│● Worker fixture    ││12:00:03      INFO   Unicode 東 京  café é request completed                  │
│  synthetic/static  ││12:00:04      INFO   fixture request 04 completed                           │
│   Errors only      ││12:00:05      WARN   fixture request 05 completed                           │
│                    ││12:00:06      INFO   fixture request 06 completed                           │
│         ┌ Search ──────────────────────────────────────────────────────────────────────┐         │
│         │ level: WARN                                                                  │         │
│         │ Applied  No filter applied.                                                  │         │
│         │                                                                              │         │
│         │                                                                              │         │
│         │                                                                              │         │
│         │                                                                              │         │
│         │ Examples: text · "field name": text · /regex/ims · \/literal                 │         │
│         │                                                                              │         │
│         │                                                                              │         │
│         └──────────────────────────────────────────────────────────────────────────────┘         │
│                    ││                                                                            │
│                    ││                                                                            │
│                    ││                                                                            │
│                    ││                                                                            │
│                    ││                                                                            │
│                    ││                                                                            │
│                    ││                                                                            │
│                    ││                                                                            │
└────────────────────┘└────────────────────────────────────────────────────────────────────────────┘
                       FOLLOW | 1-16/16 | ? help                                                    

===== search @ 80x24 focus=SearchEditor surface=Some(Rect { x: 9, y: 7, width: 62, height: 9 }) dialog_scroll=0 help_scroll=0 scroll_hitbox=None =====
lvu log workspace DEMO FIXTURE — NOT ACQUISITION                                
┌ Sources / views ───┐┌ Log viewport ──────────────────────────────────────────┐
│● API fixture       ││time          level  event                              │
│  synthetic/live    ││12:00:01      INFO   fixture request 01 completed       │
│ › All events       ││12:00:02      INFO   fixture request 02 completed       │
│● Worker fixture    ││12:00:03      INFO   Unicode 東 京  café é request complet│
│  synth┌ Search ──────────────────────────────────────────────────────┐       │
│   Erro│ level: WARN                                                  │       │
│       │ Applied  No filter applied.                                  │       │
│       │                                                              │       │
│       │                                                              │       │
│       │                                                              │       │
│       │                                                              │       │
│       │ Examples: text · "field name": text · /regex/ims · \/literal │       │
│       │                                                              │       │
│       │                                                              │       │
│       └──────────────────────────────────────────────────────────────┘       │
│                    ││12:00:15      WARN   fixture request 15 completed       │
│                    ││12:00:16      INFO   fixture request 16 completed       │
│                    ││                                                        │
│                    ││                                                        │
│                    ││                                                        │
└────────────────────┘└────────────────────────────────────────────────────────┘
                       FOLLOW | 1-16/16 | ? help                                

===== search @ 54x16 focus=SearchEditor surface=Some(Rect { x: 6, y: 3, width: 41, height: 9 }) dialog_scroll=0 help_scroll=0 scroll_hitbox=None =====
lvu log workspace DEMO FIXTURE — NOT ACQUISITION      
┌ Sources / views ───┐┌ Log viewport ────────────────┐
│● AP┌ Search ─────────────────────────────────┐t    │
│  sy│ level: WARN                             │ure r│
│ › A│ Applied  No filter applied.             │ure r│
│● Wo│                                         │ure r│
│  sy│                                         │ure r│
│   E│                                         │ure r│
│    │                                         │ure r│
│    │ Examples: text · "field name": text ·   │ure r│
│    │ /regex/ims · \/literal                  │ure r│
│    │                                         │ure r│
│    └─────────────────────────────────────────┘ure r│
│                    ││12:00:16      INFO   fixture r│
└────────────────────┘└──────────────────────────────┘
                       FOLLOW | 6-16/16 | ? help      

===== advanced @ 140x40 focus=AdvancedEditor surface=Some(Rect { x: 15, y: 15, width: 110, height: 9 }) dialog_scroll=0 help_scroll=0 scroll_hitbox=None =====
lvu log workspace DEMO FIXTURE — NOT ACQUISITION                                                                                            
┌ Sources / views ───┐┌ Log viewport ──────────────────────────────────────────────────────────────────────────────────────────────────────┐
│● API fixture       ││time          level  event                                                                                          │
│  synthetic/live    ││12:00:01      INFO   fixture request 01 completed                                                                   │
│ › All events       ││12:00:02      INFO   fixture request 02 completed                                                                   │
│● Worker fixture    ││12:00:03      INFO   Unicode 東 京  café é request completed                                                          │
│  synthetic/static  ││12:00:04      INFO   fixture request 04 completed                                                                   │
│   Errors only      ││12:00:05      WARN   fixture request 05 completed                                                                   │
│                    ││12:00:06      INFO   fixture request 06 completed                                                                   │
│                    ││12:00:07      INFO   fixture request 07 completed                                                                   │
│                    ││12:00:08      INFO   fixture request 08 completed                                                                   │
│                    ││12:00:09      INFO   fixture request 09 completed                                                                   │
│                    ││12:00:10      WARN   fixture request 10 completed                                                                   │
│                    ││12:00:11      INFO   fixture request 11 completed                                                                   │
│             ┌ Advanced filter ─────────────────────────────────────────────────────────────────────────────────────────────┐             │
│             │ FILTER EXPRESSION                                                                                            │             │
│             │ col("level").eq(lit("WARN"))                                                                                 │             │
│             │ Applied  No filter applied.                                                                                  │             │
│             │                                                                                                              │             │
│             │                                                                                                              │             │
│             │                                                                                                              │             │
│             │ Use a Polars expression. Fields and static sampled literals are available as completions.                    │             │
│             │                                                                                                              │             │
│             │                                                                                                              │             │
│             └──────────────────────────────────────────────────────────────────────────────────────────────────────────────┘             │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
└────────────────────┘└────────────────────────────────────────────────────────────────────────────────────────────────────────────────────┘
                       FOLLOW | 1-16/16 | ? help                                                                                            

===== advanced @ 100x30 focus=AdvancedEditor surface=Some(Rect { x: 11, y: 10, width: 78, height: 9 }) dialog_scroll=0 help_scroll=0 scroll_hitbox=None =====
lvu log workspace DEMO FIXTURE — NOT ACQUISITION                                                    
┌ Sources / views ───┐┌ Log viewport ──────────────────────────────────────────────────────────────┐
│● API fixture       ││time          level  event                                                  │
│  synthetic/live    ││12:00:01      INFO   fixture request 01 completed                           │
│ › All events       ││12:00:02      INFO   fixture request 02 completed                           │
│● Worker fixture    ││12:00:03      INFO   Unicode 東 京  café é request completed                  │
│  synthetic/static  ││12:00:04      INFO   fixture request 04 completed                           │
│   Errors only      ││12:00:05      WARN   fixture request 05 completed                           │
│                    ││12:00:06      INFO   fixture request 06 completed                           │
│         ┌ Advanced filter ─────────────────────────────────────────────────────────────┐         │
│         │ FILTER EXPRESSION                                                            │         │
│         │ col("level").eq(lit("WARN"))                                                 │         │
│         │ Applied  No filter applied.                                                  │         │
│         │                                                                              │         │
│         │                                                                              │         │
│         │                                                                              │         │
│         │ Use a Polars expression. Fields and static sampled literals are available as │         │
│         │ completions.                                                                 │         │
│         │                                                                              │         │
│         └──────────────────────────────────────────────────────────────────────────────┘         │
│                    ││                                                                            │
│                    ││                                                                            │
│                    ││                                                                            │
│                    ││                                                                            │
│                    ││                                                                            │
│                    ││                                                                            │
│                    ││                                                                            │
│                    ││                                                                            │
└────────────────────┘└────────────────────────────────────────────────────────────────────────────┘
                       FOLLOW | 1-16/16 | ? help                                                    

===== advanced @ 80x24 focus=AdvancedEditor surface=Some(Rect { x: 9, y: 7, width: 62, height: 9 }) dialog_scroll=0 help_scroll=0 scroll_hitbox=None =====
lvu log workspace DEMO FIXTURE — NOT ACQUISITION                                
┌ Sources / views ───┐┌ Log viewport ──────────────────────────────────────────┐
│● API fixture       ││time          level  event                              │
│  synthetic/live    ││12:00:01      INFO   fixture request 01 completed       │
│ › All events       ││12:00:02      INFO   fixture request 02 completed       │
│● Worker fixture    ││12:00:03      INFO   Unicode 東 京  café é request complet│
│  synth┌ Advanced filter ─────────────────────────────────────────────┐       │
│   Erro│ FILTER EXPRESSION                                            │       │
│       │ col("level").eq(lit("WARN"))                                 │       │
│       │ Applied  No filter applied.                                  │       │
│       │                                                              │       │
│       │                                                              │       │
│       │                                                              │       │
│       │ Use a Polars expression. Fields and static sampled literals  │       │
│       │ are available as completions.                                │       │
│       │                                                              │       │
│       └──────────────────────────────────────────────────────────────┘       │
│                    ││12:00:15      WARN   fixture request 15 completed       │
│                    ││12:00:16      INFO   fixture request 16 completed       │
│                    ││                                                        │
│                    ││                                                        │
│                    ││                                                        │
└────────────────────┘└────────────────────────────────────────────────────────┘
                       FOLLOW | 1-16/16 | ? help                                

===== advanced @ 54x16 focus=AdvancedEditor surface=Some(Rect { x: 6, y: 3, width: 41, height: 9 }) dialog_scroll=0 help_scroll=0 scroll_hitbox=None =====
lvu log workspace DEMO FIXTURE — NOT ACQUISITION      
┌ Sources / views ───┐┌ Log viewport ────────────────┐
│● AP┌ Advanced filter ────────────────────────┐t    │
│  sy│ FILTER EXPRESSION                       │ure r│
│ › A│ col("level").eq(lit("WARN"))            │ure r│
│● Wo│ Applied  No filter applied.             │ure r│
│  sy│                                         │ure r│
│   E│                                         │ure r│
│    │                                         │ure r│
│    │ Use a Polars expression. Fields and     │ure r│
│    │ static sampled literals are available   │ure r│
│    │                                         │ure r│
│    └─────────────────────────────────────────┘ure r│
│                    ││12:00:16      INFO   fixture r│
└────────────────────┘└──────────────────────────────┘
                       FOLLOW | 6-16/16 | ? help      

===== grouping @ 140x40 focus=GroupingEditor surface=Some(Rect { x: 15, y: 14, width: 110, height: 11 }) dialog_scroll=0 help_scroll=0 scroll_hitbox=None =====
lvu log workspace DEMO FIXTURE — NOT ACQUISITION                                                                                            
┌ Sources / views ───┐┌ Log viewport ──────────────────────────────────────────────────────────────────────────────────────────────────────┐
│● API fixture       ││time          level  event                                                                                          │
│  synthetic/live    ││12:00:01      INFO   fixture request 01 completed                                                                   │
│ › All events       ││12:00:02      INFO   fixture request 02 completed                                                                   │
│● Worker fixture    ││12:00:03      INFO   Unicode 東 京  café é request completed                                                          │
│  synthetic/static  ││12:00:04      INFO   fixture request 04 completed                                                                   │
│   Errors only      ││12:00:05      WARN   fixture request 05 completed                                                                   │
│                    ││12:00:06      INFO   fixture request 06 completed                                                                   │
│                    ││12:00:07      INFO   fixture request 07 completed                                                                   │
│                    ││12:00:08      INFO   fixture request 08 completed                                                                   │
│                    ││12:00:09      INFO   fixture request 09 completed                                                                   │
│                    ││12:00:10      WARN   fixture request 10 completed                                                                   │
│             ┌ Display-only multiline grouping ─────────────────────────────────────────────────────────────────────────────┐             │
│             │ Continuation regex over raw bytes                                                                            │             │
│             │ ^(\s+|Caused by:)                                                                                            │             │
│             │ Applied: Grouping disabled.                                                                                  │             │
│             │                                                                                                              │             │
│             │                                                                                                              │             │
│             │                                                                                                              │             │
│             │ Preview (display only):                                                                                      │             │
│             │ RuntimeException: boom                                                                                       │             │
│             │   at worker.rs:42  → 2 physical lines                                                                        │             │
│             │ Empty draft disables grouping                                                                                │             │
│             │                                                                                                              │             │
│             └──────────────────────────────────────────────────────────────────────────────────────────────────────────────┘             │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
└────────────────────┘└────────────────────────────────────────────────────────────────────────────────────────────────────────────────────┘
                       FOLLOW | 1-16/16 | ? help                                                                                            

===== grouping @ 100x30 focus=GroupingEditor surface=Some(Rect { x: 11, y: 9, width: 78, height: 11 }) dialog_scroll=0 help_scroll=0 scroll_hitbox=None =====
lvu log workspace DEMO FIXTURE — NOT ACQUISITION                                                    
┌ Sources / views ───┐┌ Log viewport ──────────────────────────────────────────────────────────────┐
│● API fixture       ││time          level  event                                                  │
│  synthetic/live    ││12:00:01      INFO   fixture request 01 completed                           │
│ › All events       ││12:00:02      INFO   fixture request 02 completed                           │
│● Worker fixture    ││12:00:03      INFO   Unicode 東 京  café é request completed                  │
│  synthetic/static  ││12:00:04      INFO   fixture request 04 completed                           │
│   Errors only      ││12:00:05      WARN   fixture request 05 completed                           │
│         ┌ Display-only multiline grouping ─────────────────────────────────────────────┐         │
│         │ Continuation regex over raw bytes                                            │         │
│         │ ^(\s+|Caused by:)                                                            │         │
│         │ Applied: Grouping disabled.                                                  │         │
│         │                                                                              │         │
│         │                                                                              │         │
│         │                                                                              │         │
│         │ Preview (display only):                                                      │         │
│         │ RuntimeException: boom                                                       │         │
│         │   at worker.rs:42  → 2 physical lines                                        │         │
│         │ Empty draft disables grouping                                                │         │
│         │                                                                              │         │
│         └──────────────────────────────────────────────────────────────────────────────┘         │
│                    ││                                                                            │
│                    ││                                                                            │
│                    ││                                                                            │
│                    ││                                                                            │
│                    ││                                                                            │
│                    ││                                                                            │
│                    ││                                                                            │
└────────────────────┘└────────────────────────────────────────────────────────────────────────────┘
                       FOLLOW | 1-16/16 | ? help                                                    

===== grouping @ 80x24 focus=GroupingEditor surface=Some(Rect { x: 9, y: 6, width: 62, height: 11 }) dialog_scroll=0 help_scroll=0 scroll_hitbox=None =====
lvu log workspace DEMO FIXTURE — NOT ACQUISITION                                
┌ Sources / views ───┐┌ Log viewport ──────────────────────────────────────────┐
│● API fixture       ││time          level  event                              │
│  synthetic/live    ││12:00:01      INFO   fixture request 01 completed       │
│ › All events       ││12:00:02      INFO   fixture request 02 completed       │
│● Worke┌ Display-only multiline grouping ─────────────────────────────┐complet│
│  synth│ Continuation regex over raw bytes                            │       │
│   Erro│ ^(\s+|Caused by:)                                            │       │
│       │ Applied: Grouping disabled.                                  │       │
│       │                                                              │       │
│       │                                                              │       │
│       │                                                              │       │
│       │ Preview (display only):                                      │       │
│       │ RuntimeException: boom                                       │       │
│       │   at worker.rs:42  → 2 physical lines                        │       │
│       │ Empty draft disables grouping                                │       │
│       │                                                              │       │
│       └──────────────────────────────────────────────────────────────┘       │
│                    ││12:00:16      INFO   fixture request 16 completed       │
│                    ││                                                        │
│                    ││                                                        │
│                    ││                                                        │
└────────────────────┘└────────────────────────────────────────────────────────┘
                       FOLLOW | 1-16/16 | ? help                                

===== grouping @ 54x16 focus=GroupingEditor surface=Some(Rect { x: 6, y: 2, width: 41, height: 11 }) dialog_scroll=0 help_scroll=0 scroll_hitbox=None =====
lvu log workspace DEMO FIXTURE — NOT ACQUISITION      
┌ Sou┌ Display-only multiline grouping ────────┐─────┐
│● AP│ Continuation regex over raw bytes       │t    │
│  sy│ ^(\s+|Caused by:)                       │ure r│
│ › A│ Applied: Grouping disabled.             │ure r│
│● Wo│                                         │ure r│
│  sy│                                         │ure r│
│   E│                                         │ure r│
│    │ Preview (display only):                 │ure r│
│    │ RuntimeException: boom                  │ure r│
│    │   at worker.rs:42  → 2 physical lines   │ure r│
│    │ Empty draft disables grouping           │ure r│
│    │                                         │ure r│
│    └─────────────────────────────────────────┘ure r│
└────────────────────┘└──────────────────────────────┘
                       FOLLOW | 6-16/16 | ? help      

===== time @ 140x40 focus=TimeEditor surface=Some(Rect { x: 9, y: 10, width: 121, height: 20 }) dialog_scroll=0 help_scroll=0 scroll_hitbox=None =====
lvu log workspace DEMO FIXTURE — NOT ACQUISITION                                                                                            
┌ Sources / views ───┐┌ Log viewport ──────────────────────────────────────────────────────────────────────────────────────────────────────┐
│● API fixture       ││time          level  event                                                                                          │
│  synthetic/live    ││12:00:01      INFO   fixture request 01 completed                                                                   │
│ › All events       ││12:00:02      INFO   fixture request 02 completed                                                                   │
│● Worker fixture    ││12:00:03      INFO   Unicode 東 京  café é request completed                                                          │
│  synthetic/static  ││12:00:04      INFO   fixture request 04 completed                                                                   │
│   Errors only      ││12:00:05      WARN   fixture request 05 completed                                                                   │
│                    ││12:00:06      INFO   fixture request 06 completed                                                                   │
│       ┌ Time window ────────────────────────────────────────────────────────────────────────────────────────────────────────────┐        │
│       │                                                                                                                         │        │
│       │ [ Time basis: Capture ▾ ]                                                                                               │        │
│       │ [ Window: All time ▾ ]                                                                                                  │        │
│       │                                                                                                                         │        │
│       │ Start 1969-12-31 23:59:46.000000000                                                                      UTC      [ ▾ ] │        │
│       │ End   1970-01-01 00:00:46.000000000                                                                      UTC      [ ▾ ] │        │
│       │                                                                                                                         │        │
│       │ [ Apply ] [ Clear ] [ 🧠  Recognize timestamp ]                                                                          │        │
│       │                                                                                                                         │        │
│       │ ┌ Applied ────────────────────────────────────────────────────────────────────────────────────────────────────────────┐ │        │
│       │ │Applied: all times                                                                                                   │ │        │
│       │ └─────────────────────────────────────────────────────────────────────────────────────────────────────────────────────┘ │        │
│       │                                                                                                                         │        │
│       │ Bounds are half-open. UTC and numeric offsets are normalized to UTC; named zones are not supported.                     │        │
│       │                                                                                                                         │        │
│       │                                                                                                                         │        │
│       │                                                                                                                         │        │
│       │                                                                                                                         │        │
│       │                                                                                                                         │        │
│       │                                                                                                                         │        │
│       └─────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────┘        │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
└────────────────────┘└────────────────────────────────────────────────────────────────────────────────────────────────────────────────────┘
                       FOLLOW | 1-16/16 | ? help                                                                                            

===== time @ 100x30 focus=TimeEditor surface=Some(Rect { x: 7, y: 5, width: 86, height: 20 }) dialog_scroll=0 help_scroll=0 scroll_hitbox=None =====
lvu log workspace DEMO FIXTURE — NOT ACQUISITION                                                    
┌ Sources / views ───┐┌ Log viewport ──────────────────────────────────────────────────────────────┐
│● API fixture       ││time          level  event                                                  │
│  synthetic/live    ││12:00:01      INFO   fixture request 01 completed                           │
│ › Al┌ Time window ─────────────────────────────────────────────────────────────────────────┐     │
│● Wor│                                                                                      │     │
│  syn│ [ Time basis: Capture ▾ ]                                                            │     │
│   Er│ [ Window: All time ▾ ]                                                               │     │
│     │                                                                                      │     │
│     │ Start 1969-12-31 23:59:46.000000000                                   UTC      [ ▾ ] │     │
│     │ End   1970-01-01 00:00:46.000000000                                   UTC      [ ▾ ] │     │
│     │                                                                                      │     │
│     │ [ Apply ] [ Clear ] [ 🧠  Recognize timestamp ]                                       │     │
│     │                                                                                      │     │
│     │ ┌ Applied ─────────────────────────────────────────────────────────────────────────┐ │     │
│     │ │Applied: all times                                                                │ │     │
│     │ └──────────────────────────────────────────────────────────────────────────────────┘ │     │
│     │                                                                                      │     │
│     │ Bounds are half-open. UTC and numeric offsets are normalized to UTC; named zones are │     │
│     │  not supported.                                                                      │     │
│     │                                                                                      │     │
│     │                                                                                      │     │
│     │                                                                                      │     │
│     │                                                                                      │     │
│     │                                                                                      │     │
│     └──────────────────────────────────────────────────────────────────────────────────────┘     │
│                    ││                                                                            │
│                    ││                                                                            │
└────────────────────┘└────────────────────────────────────────────────────────────────────────────┘
                       FOLLOW | 1-16/16 | ? help                                                    

===== time @ 80x24 focus=TimeEditor surface=Some(Rect { x: 6, y: 2, width: 68, height: 20 }) dialog_scroll=0 help_scroll=0 scroll_hitbox=None =====
lvu log workspace DEMO FIXTURE — NOT ACQUISITION                                
┌ Sou┌ Time window ───────────────────────────────────────────────────────┐────┐
│● AP│                                                                    │    │
│  sy│ [ Time basis: Capture ▾ ]                                          │    │
│ › A│ [ Window: All time ▾ ]                                             │    │
│● Wo│                                                                    │plet│
│  sy│ Start 1969-12-31 23:59:46.000000000                 UTC      [ ▾ ] │    │
│   E│ End   1970-01-01 00:00:46.000000000                 UTC      [ ▾ ] │    │
│    │                                                                    │    │
│    │ [ Apply ] [ Clear ] [ 🧠  Recognize timestamp ]                     │    │
│    │                                                                    │    │
│    │ ┌ Applied ───────────────────────────────────────────────────────┐ │    │
│    │ │Applied: all times                                              │ │    │
│    │ └────────────────────────────────────────────────────────────────┘ │    │
│    │                                                                    │    │
│    │ Bounds are half-open. UTC and numeric offsets are normalized to UT │    │
│    │ C; named zones are not supported.                                  │    │
│    │                                                                    │    │
│    │                                                                    │    │
│    │                                                                    │    │
│    │                                                                    │    │
│    │                                                                    │    │
└────└────────────────────────────────────────────────────────────────────┘────┘
                       FOLLOW | 1-16/16 | ? help                                

===== time @ 54x16 focus=TimeEditor surface=Some(Rect { x: 4, y: 1, width: 45, height: 14 }) dialog_scroll=0 help_scroll=0 scroll_hitbox=None =====
lvu┌ Time window ────────────────────────────────┐    
┌ S│ [ ▲ Scroll up ]                             │───┐
│● │ [ Time basis: Capture ▾ ]                   │   │
│  │ [ Window: All time ▾ ]                      │e r│
│ ›│                                             │e r│
│● │ Start date  1969-12-31                      │e r│
│  │ time  23:59:46.000000000                    │e r│
│  │ zone  UTC                             [ ▾ ] │e r│
│  │ End date  1970-01-01                        │e r│
│  │ time  00:00:46.000000000                    │e r│
│  │ zone  UTC                             [ ▾ ] │e r│
│  │                                             │e r│
│  │ [ Apply ] [ Clear ]                         │e r│
│  │ [ 🧠  Recognize timestamp ]                  │e r│
└──│ [ ▼ Scroll down ]                           │───┘
   └─────────────────────────────────────────────┘    

===== settings @ 140x40 focus=Settings surface=Some(Rect { x: 1, y: 6, width: 138, height: 28 }) dialog_scroll=0 help_scroll=0 scroll_hitbox=None =====
lvu log workspace DEMO FIXTURE — NOT ACQUISITION                                                                                            
┌ Sources / views ───┐┌ Log viewport ──────────────────────────────────────────────────────────────────────────────────────────────────────┐
│● API fixture       ││time          level  event                                                                                          │
│  synthetic/live    ││12:00:01      INFO   fixture request 01 completed                                                                   │
│ › All events       ││12:00:02      INFO   fixture request 02 completed                                                                   │
┌ Settings ────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────┐
│ 🧠  configuration                                                                                                                         │
│ Provider/model: codex/gpt-5.6-sol            Mode: full-access                             Thinking: medium                              │
│                                                                                                                                          │
│ Appearance                                                                                                                               │
│ [ Theme: terminal ▾ ] [ Delight: On ] [ Reduced motion: Off ] [ ASCII: Off ]                                                             │
│                                                                                                                                          │
│                                                                                                                                          │
│ Cache limits (MiB)                                                                                                                       │
│ Rows: 4                                                             Membership: 256                                                      │
│ Derived total: 5120                                                 Per source: 256                                                      │
│ [ Save ]                                                                                                                                 │
│ ┌ State ───────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────┐ │
│ │Saved: Saved; restart required for cache-limit changes                                                                                │ │
│ │Saved settings loaded; cache-limit changes apply after restart                                                                        │ │
│ └──────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────┘ │
│ ┌ Effective values and paths ──────────────────────────────────────────────────────────────────────────────────────────────────────────┐ │
│ │Effective 🧠 : codex/env [environment LVU_AI_PROVIDER] · full-access [settings.toml] · medium [settings.toml]                          │ │
│ │Effective appearance: theme terminal · delight false [environment LVU_NO_DELIGHT] · motion true [environment LVU_REDUCED_MOTION] ·    │ │
│ │ASCII false [settings.toml]                                                                                                           │ │
│ │Startup-applied MiB: rows 4 · membership 256 · total derived 5120 · index/source 256                                                  │ │
│ │Settings: /home/user/.config/lvu/settings.toml                                                                                        │ │
│ │Data: /home/user/.local/share/lvu                                                                                                     │ │
│ │Cache: /home/user/.cache/lvu                                                                                                          │ │
│ │Capture: /home/user/.local/share/lvu/captures                                                                                         │ │
│ │Cache-limit changes take effect after restart; appearance previews immediately.                                                       │ │
│ │                                                                                                                                      │ │
│ └──────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────┘ │
│                                                                                                                                          │
└──────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────┘
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
└────────────────────┘└────────────────────────────────────────────────────────────────────────────────────────────────────────────────────┘
                       FOLLOW | 1-16/16 | ? help                                                                                            

===== settings @ 100x30 focus=Settings surface=Some(Rect { x: 1, y: 1, width: 98, height: 28 }) dialog_scroll=0 help_scroll=0 scroll_hitbox=None =====
┌ Settings ────────────────────────────────────────────────────────────────────────────────────────┐
│ 🧠  configuration                                                                                 │
│ Provider/model: ex/gpt-5.6-sol  Mode: full-access               Thinking: medium                 │
│                                                                                                  │
│ Appearance                                                                                       │
│ [ Theme: terminal ▾ ] [ Delight: On ] [ Reduced motion: Off ] [ ASCII: Off ]                     │
│                                                                                                  │
│                                                                                                  │
│ Cache limits (MiB)                                                                               │
│ Rows: 4                                         Membership: 256                                  │
│ Derived total: 5120                             Per source: 256                                  │
│ [ Save ]                                                                                         │
│ ┌ State ───────────────────────────────────────────────────────────────────────────────────────┐ │
│ │Saved: Saved; restart required for cache-limit changes                                        │ │
│ │Saved settings loaded; cache-limit changes apply after restart                                │ │
│ └──────────────────────────────────────────────────────────────────────────────────────────────┘ │
│ ┌ Effective values and paths ──────────────────────────────────────────────────────────────────┐ │
│ │Effective 🧠 : codex/env [environment LVU_AI_PROVIDER] · full-access [settings.toml] · medium  │ │
│ │[settings.toml]                                                                               │ │
│ │Effective appearance: theme terminal · delight false [environment LVU_NO_DELIGHT] · motion    │ │
│ │true [environment LVU_REDUCED_MOTION] · ASCII false [settings.toml]                           │ │
│ │Startup-applied MiB: rows 4 · membership 256 · total derived 5120 · index/source 256          │ │
│ │Settings: /home/user/.config/lvu/settings.toml                                                │ │
│ │Data: /home/user/.local/share/lvu                                                             │ │
│ │Cache: /home/user/.cache/lvu                                                                  │ │
│ │Capture: /home/user/.local/share/lvu/captures                                                 │ │
│ │Cache-limit changes take effect after restart; appearance previews immediately.               │ │
│ └──────────────────────────────────────────────────────────────────────────────────────────────┘ │
│                                                                                                  │
└──────────────────────────────────────────────────────────────────────────────────────────────────┘

===== settings @ 80x24 focus=Settings surface=Some(Rect { x: 1, y: 1, width: 78, height: 22 }) dialog_scroll=0 help_scroll=0 scroll_hitbox=None =====
┌ Settings ────────────────────────────────────────────────────────────────────┐
│ 🧠  configuration                                                             │
│ Provider/model: codex/gpt-5.6-sol                                            │
│ Mode: full-access                                                            │
│ Thinking: medium                                                             │
│ Appearance                                                                   │
│ [ Theme: terminal ▾ ] [ Delight: On ] [ Reduced motion: Off ] [ ASCII: Off ] │
│                                                                              │
│                                                                              │
│ Cache limits (MiB)                                                           │
│ Rows: 4                                                                      │
│ Membership: 256                                                              │
│ Derived total: 5120                                                          │
│ Per source: 256                                                              │
│ [ Save ] [ More ]                                                            │
│ ┌ State ───────────────────────────────────────────────────────────────────┐ │
│ │Saved: Saved; restart required for cache-limit changes                    │ │
│ │Saved settings loaded; cache-limit changes apply after restart            │ │
│ └──────────────────────────────────────────────────────────────────────────┘ │
│ ┌ Effective values and paths ──────────────────────────────────────────────┐ │
│ │Effective 🧠 : codex/env [environment LVU_AI_PROVIDER] · full-access       │ │
│ └──────────────────────────────────────────────────────────────────────────┘ │
│                                                                              │
└──────────────────────────────────────────────────────────────────────────────┘

===== settings @ 54x16 focus=Settings surface=Some(Rect { x: 1, y: 1, width: 52, height: 14 }) dialog_scroll=0 help_scroll=0 scroll_hitbox=None =====
┌ Settings ──────────────────────────────────────────┐
│ Provider/model                                     │
│ Value: codex/gpt-5.6-sol                           │
│ [ Save ] [ More ]                                  │
│ ┌ State ─────────────────────────────────────────┐ │
│ │Saved: Saved; restart required for cache-limit  │ │
│ │changes                                         │ │
│ └────────────────────────────────────────────────┘ │
│ ┌ Effective values and paths ────────────────────┐ │
│ │State detail: Saved settings loaded; cache-limit│ │
│ │changes apply after restart                     │ │
│ │Effective 🧠 : codex/env [environment            │ │
│ │LVU_AI_PROVIDER] · full-access [settings.toml] ·│ │
│ └────────────────────────────────────────────────┘ │
│                                                    │
└────────────────────────────────────────────────────┘

===== storage @ 140x40 focus=Storage surface=Some(Rect { x: 9, y: 11, width: 121, height: 18 }) dialog_scroll=1 help_scroll=0 scroll_hitbox=Some(Rect { x: 10, y: 25, width: 119, height: 3 }) =====
lvu log workspace DEMO FIXTURE — NOT ACQUISITION                                                                                            
┌ Sources / views ───┐┌ Log viewport ──────────────────────────────────────────────────────────────────────────────────────────────────────┐
│● API fixture       ││time          level  event                                                                                          │
│  synthetic/live    ││12:00:01      INFO   fixture request 01 completed                                                                   │
│ › All events       ││12:00:02      INFO   fixture request 02 completed                                                                   │
│● Worker fixture    ││12:00:03      INFO   Unicode 東 京  café é request completed                                                          │
│  synthetic/static  ││12:00:04      INFO   fixture request 04 completed                                                                   │
│   Errors only      ││12:00:05      WARN   fixture request 05 completed                                                                   │
│                    ││12:00:06      INFO   fixture request 06 completed                                                                   │
│                    ││12:00:07      INFO   fixture request 07 completed                                                                   │
│       ┌ Storage usage — total 50.0 MiB / unused derived 20.0 MiB ───────────────────────────────────────────────────────────────┐        │
│       │ row cache 4.0 MiB / 4.0 MiB   query membership 2.0 MiB / 256.0 MiB                                                      │        │
│       │ derived disk cap/source 256.0 MiB · global 5.0 GiB                                                                      │        │
│       │ managed budgets; not a process RSS limit                                                                                │        │
│       │                                                                                                                         │        │
│       │ > derived     1.0 MiB api-fixture/00.rows.idx — unused, recomputable from capture                                       │        │
│       │   capture     2.0 MiB api-fixture/01.rows.idx — unused, recomputable from capture reclaimable                           │        │
│       │   derived     3.0 MiB api-fixture/02.rows.idx — unused, recomputable from capture reclaimable                           │        │
│       │   capture     4.0 MiB api-fixture/03.rows.idx — unused, recomputable from capture reclaimable                           │        │
│       │   derived     5.0 MiB api-fixture/04.rows.idx — unused, recomputable from capture reclaimable                           │        │
│       │   capture     6.0 MiB api-fixture/05.rows.idx — unused, recomputable from capture reclaimable                           │        │
│       │   derived     7.0 MiB api-fixture/06.rows.idx — unused, recomputable from capture reclaimable                           │        │
│       │   capture     8.0 MiB api-fixture/07.rows.idx — unused, recomputable from capture reclaimable                           │        │
│       │   derived     9.0 MiB api-fixture/08.rows.idx — unused, recomputable from capture reclaimable                           │        │
│       │                                                                                                                         │        │
│       │ ┌ Status ─────────────────────────────────────────────────────────────────────────────────────────────────────────────┐ │        │
│       │ │Status: complete                                                                                                     │ │        │
│       │ └─────────────────────────────────────────────────────────────────────────────────────────────────────────────────────┘ │        │
│       │↑/↓ active pane · r refresh · c preview/confirm cleanup                                                                  │        │
│       └─────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────┘        │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
└────────────────────┘└────────────────────────────────────────────────────────────────────────────────────────────────────────────────────┘
                       FOLLOW | 1-16/16 | ? help                                                                                            

===== storage @ 100x30 focus=Storage surface=Some(Rect { x: 7, y: 6, width: 86, height: 18 }) dialog_scroll=2 help_scroll=0 scroll_hitbox=Some(Rect { x: 8, y: 20, width: 84, height: 3 }) =====
lvu log workspace DEMO FIXTURE — NOT ACQUISITION                                                    
┌ Sources / views ───┐┌ Log viewport ──────────────────────────────────────────────────────────────┐
│● API fixture       ││time          level  event                                                  │
│  synthetic/live    ││12:00:01      INFO   fixture request 01 completed                           │
│ › All events       ││12:00:02      INFO   fixture request 02 completed                           │
│● Wor┌ Storage usage — total 50.0 MiB / unused derived 20.0 MiB ────────────────────────────┐     │
│  syn│ row cache 4.0 MiB / 4.0 MiB   query membership 2.0 MiB / 256.0 MiB                   │     │
│   Er│ derived disk cap/source 256.0 MiB · global 5.0 GiB                                   │     │
│     │ managed budgets; not a process RSS limit                                             │     │
│     │                                                                                      │     │
│     │ > derived     1.0 MiB api-fixture/00.rows.idx — unused, recomputable from capture    │     │
│     │   capture     2.0 MiB api-fixture/01.rows.idx — unused, recomputable from capture re │     │
│     │   derived     3.0 MiB api-fixture/02.rows.idx — unused, recomputable from capture re │     │
│     │   capture     4.0 MiB api-fixture/03.rows.idx — unused, recomputable from capture re │     │
│     │   derived     5.0 MiB api-fixture/04.rows.idx — unused, recomputable from capture re │     │
│     │   capture     6.0 MiB api-fixture/05.rows.idx — unused, recomputable from capture re │     │
│     │   derived     7.0 MiB api-fixture/06.rows.idx — unused, recomputable from capture re │     │
│     │   capture     8.0 MiB api-fixture/07.rows.idx — unused, recomputable from capture re │     │
│     │   derived     9.0 MiB api-fixture/08.rows.idx — unused, recomputable from capture re │     │
│     │                                                                                      │     │
│     │ ┌ Status ──────────────────────────────────────────────────────────────────────────┐ │     │
│     │ │Status: complete                                                                  │ │     │
│     │ └──────────────────────────────────────────────────────────────────────────────────┘ │     │
│     │↑/↓ active pane · r refresh · c preview/confirm cleanup                               │     │
│     └──────────────────────────────────────────────────────────────────────────────────────┘     │
│                    ││                                                                            │
│                    ││                                                                            │
│                    ││                                                                            │
└────────────────────┘└────────────────────────────────────────────────────────────────────────────┘
                       FOLLOW | 1-16/16 | ? help                                                    

===== storage @ 80x24 focus=Storage surface=Some(Rect { x: 6, y: 3, width: 68, height: 18 }) dialog_scroll=3 help_scroll=0 scroll_hitbox=Some(Rect { x: 7, y: 17, width: 66, height: 3 }) =====
lvu log workspace DEMO FIXTURE — NOT ACQUISITION                                
┌ Sources / views ───┐┌ Log viewport ──────────────────────────────────────────┐
│● AP┌ Storage usage — total 50.0 MiB / unused derived 20.0 MiB ──────────┐    │
│  sy│ row cache 4.0 MiB / 4.0 MiB   query membership 2.0 MiB / 256.0 MiB │    │
│ › A│ derived disk cap/source 256.0 MiB · global 5.0 GiB                 │    │
│● Wo│ managed budgets; not a process RSS limit                           │plet│
│  sy│                                                                    │    │
│   E│ > derived     1.0 MiB api-fixture/00.rows.idx — unused, recomputab │    │
│    │   capture     2.0 MiB api-fixture/01.rows.idx — unused, recomputab │    │
│    │   derived     3.0 MiB api-fixture/02.rows.idx — unused, recomputab │    │
│    │   capture     4.0 MiB api-fixture/03.rows.idx — unused, recomputab │    │
│    │   derived     5.0 MiB api-fixture/04.rows.idx — unused, recomputab │    │
│    │   capture     6.0 MiB api-fixture/05.rows.idx — unused, recomputab │    │
│    │   derived     7.0 MiB api-fixture/06.rows.idx — unused, recomputab │    │
│    │   capture     8.0 MiB api-fixture/07.rows.idx — unused, recomputab │    │
│    │   derived     9.0 MiB api-fixture/08.rows.idx — unused, recomputab │    │
│    │                                                                    │    │
│    │ ┌ Status ────────────────────────────────────────────────────────┐ │    │
│    │ │Status: complete                                                │ │    │
│    │ └────────────────────────────────────────────────────────────────┘ │    │
│    │↑/↓ active pane · r refresh · c preview/confirm cleanup             │    │
│    └────────────────────────────────────────────────────────────────────┘    │
└────────────────────┘└────────────────────────────────────────────────────────┘
                       FOLLOW | 1-16/16 | ? help                                

===== storage @ 54x16 focus=Storage surface=Some(Rect { x: 4, y: 1, width: 45, height: 14 }) dialog_scroll=3 help_scroll=0 scroll_hitbox=Some(Rect { x: 5, y: 10, width: 43, height: 3 }) =====
lvu┌ Storage usage — total 50.0 MiB / unused deri┐    
┌ S│ row cache 4.0 MiB / 4.0 MiB   query members │───┐
│● │ derived disk cap/source 256.0 MiB · global  │   │
│  │ managed budgets; not a process RSS limit    │e r│
│ ›│                                             │e r│
│● │ > derived     1.0 MiB api-fixture/00.rows.i │e r│
│  │   capture     2.0 MiB api-fixture/01.rows.i │e r│
│  │   derived     3.0 MiB api-fixture/02.rows.i │e r│
│  │   capture     4.0 MiB api-fixture/03.rows.i │e r│
│  │   derived     5.0 MiB api-fixture/04.rows.i │e r│
│  │ ┌ Status ─────────────────────────────────┐ │e r│
│  │ │Status: complete                         │ │e r│
│  │ └─────────────────────────────────────────┘ │e r│
│  │↑/↓ active pane · r refresh · c              │e r│
└──│preview/confirm cleanup                      │───┘
   └─────────────────────────────────────────────┘    

===== recipes @ 140x40 focus=Recipes surface=Some(Rect { x: 12, y: 11, width: 115, height: 18 }) dialog_scroll=0 help_scroll=0 scroll_hitbox=None =====
lvu log workspace DEMO FIXTURE — NOT ACQUISITION                                                                                            
┌ Sources / views ───┐┌ Log viewport ──────────────────────────────────────────────────────────────────────────────────────────────────────┐
│● API fixture       ││time          level  event                                                                                          │
│  synthetic/live    ││12:00:01      INFO   fixture request 01 completed                                                                   │
│ › All events       ││12:00:02      INFO   fixture request 02 completed                                                                   │
│● Worker fixture    ││12:00:03      INFO   Unicode 東 京  café é request completed                                                          │
│  synthetic/static  ││12:00:04      INFO   fixture request 04 completed                                                                   │
│   Errors only      ││12:00:05      WARN   fixture request 05 completed                                                                   │
│                    ││12:00:06      INFO   fixture request 06 completed                                                                   │
│                    ││12:00:07      INFO   fixture request 07 completed                                                                   │
│          ┌ Named recipes ────────────────────────────────────────────────────────────────────────────────────────────────────┐           │
│          │ > weekly error triage 0 @ rev00000                                                                                │           │
│          │   weekly error triage 1 @ rev10000                                                                                │           │
│          │   weekly error triage 2 @ rev20000                                                                                │           │
│          │   weekly error triage 3 @ rev30000                                                                                │           │
│          │   weekly error triage 4 @ rev40000                                                                                │           │
│          │   weekly error triage 5 @ rev50000                                                                                │           │
│          │ No applicable similar-source suggestions; all recipes remain browsable.                                           │           │
│          │ Preview search="" advanced=false enrichment=false pins= color=none capture-time=all                               │           │
│          │ Applied: 6 saved recipes · Enter applies the selected revision                                                    │           │
│          │                                                                                                                   │           │
│          │                                                                                                                   │           │
│          │                                                                                                                   │           │
│          │                                                                                                                   │           │
│          │                                                                                                                   │           │
│          │                                                                                                                   │           │
│          │[ Browse ] [ Save ] [ Import ] [ Export ] [ History ] [ Update ] [ Refresh ] [ Apply revision ]                    │           │
│          │                                                                                                                   │           │
│          │                                                                                                                   │           │
│          └───────────────────────────────────────────────────────────────────────────────────────────────────────────────────┘           │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
└────────────────────┘└────────────────────────────────────────────────────────────────────────────────────────────────────────────────────┘
                       FOLLOW | 1-16/16 | ? help                                                                                            

===== recipes @ 100x30 focus=Recipes surface=Some(Rect { x: 9, y: 6, width: 82, height: 18 }) dialog_scroll=0 help_scroll=0 scroll_hitbox=None =====
lvu log workspace DEMO FIXTURE — NOT ACQUISITION                                                    
┌ Sources / views ───┐┌ Log viewport ──────────────────────────────────────────────────────────────┐
│● API fixture       ││time          level  event                                                  │
│  synthetic/live    ││12:00:01      INFO   fixture request 01 completed                           │
│ › All events       ││12:00:02      INFO   fixture request 02 completed                           │
│● Worke┌ Named recipes ───────────────────────────────────────────────────────────────────┐       │
│  synth│ > weekly error triage 0 @ rev00000                                               │       │
│   Erro│   weekly error triage 1 @ rev10000                                               │       │
│       │   weekly error triage 2 @ rev20000                                               │       │
│       │   weekly error triage 3 @ rev30000                                               │       │
│       │   weekly error triage 4 @ rev40000                                               │       │
│       │   weekly error triage 5 @ rev50000                                               │       │
│       │ No applicable similar-source suggestions; all recipes remain browsable.          │       │
│       │ Preview search="" advanced=false enrichment=false pins= color=none               │       │
│       │ capture-time=all                                                                 │       │
│       │ Applied: 6 saved recipes · Enter applies the selected revision                   │       │
│       │                                                                                  │       │
│       │                                                                                  │       │
│       │                                                                                  │       │
│       │                                                                                  │       │
│       │                                                                                  │       │
│       │[ Browse ] [ Save ] [ Import ] [ Export ] [ History ] [ Update ] [ Refresh ]      │       │
│       │[ Apply revision ]                                                                │       │
│       │                                                                                  │       │
│       └──────────────────────────────────────────────────────────────────────────────────┘       │
│                    ││                                                                            │
│                    ││                                                                            │
│                    ││                                                                            │
└────────────────────┘└────────────────────────────────────────────────────────────────────────────┘
                       FOLLOW | 1-16/16 | ? help                                                    

===== recipes @ 80x24 focus=Recipes surface=Some(Rect { x: 7, y: 3, width: 65, height: 18 }) dialog_scroll=0 help_scroll=0 scroll_hitbox=None =====
lvu log workspace DEMO FIXTURE — NOT ACQUISITION                                
┌ Sources / views ───┐┌ Log viewport ──────────────────────────────────────────┐
│● API┌ Named recipes ──────────────────────────────────────────────────┐      │
│  syn│ > weekly error triage 0 @ rev00000                              │      │
│ › Al│   weekly error triage 1 @ rev10000                              │      │
│● Wor│   weekly error triage 2 @ rev20000                              │omplet│
│  syn│   weekly error triage 3 @ rev30000                              │      │
│   Er│   weekly error triage 4 @ rev40000                              │      │
│     │   weekly error triage 5 @ rev50000                              │      │
│     │ No applicable similar-source suggestions; all recipes remain    │      │
│     │ browsable.                                                      │      │
│     │ Preview search="" advanced=false enrichment=false pins=         │      │
│     │ color=none capture-time=all                                     │      │
│     │ Applied: 6 saved recipes · Enter applies the selected revision  │      │
│     │                                                                 │      │
│     │                                                                 │      │
│     │                                                                 │      │
│     │                                                                 │      │
│     │[ Browse ] [ Save ] [ Import ] [ Export ] [ History ] [ Update ] │      │
│     │[ Refresh ] [ Apply revision ]                                   │      │
│     │                                                                 │      │
│     └─────────────────────────────────────────────────────────────────┘      │
└────────────────────┘└────────────────────────────────────────────────────────┘
                       FOLLOW | 1-16/16 | ? help                                

===== recipes @ 54x16 focus=Recipes surface=Some(Rect { x: 5, y: 1, width: 43, height: 14 }) dialog_scroll=0 help_scroll=0 scroll_hitbox=None =====
lvu ┌ Named recipes ────────────────────────────┐     
┌ So│ > weekly error triage 0 @ rev00000        │────┐
│● A│   weekly error triage 1 @ rev10000        │    │
│  s│   weekly error triage 2 @ rev20000        │re r│
│ › │   weekly error triage 3 @ rev30000        │re r│
│● W│   weekly error triage 4 @ rev40000        │re r│
│  s│ No applicable similar-source suggestions; │re r│
│   │ all recipes remain browsable.             │re r│
│   │ Preview search="" advanced=false          │re r│
│   │ enrichment=false pins= color=none         │re r│
│   │ capture-time=all                          │re r│
│   │ Applied: 6 saved recipes · Enter applies  │re r│
│   │[ Browse ] [ Save ] [ Import ] [ Export ]  │re r│
│   │[ History ] [ Update ] [ Refresh ]         │re r│
└───│[ Apply revision ]                         │────┘
    └───────────────────────────────────────────┘     

===== bookmarks @ 140x40 focus=Bookmarks surface=Some(Rect { x: 1, y: 10, width: 138, height: 20 }) dialog_scroll=0 help_scroll=0 scroll_hitbox=None =====
lvu log workspace DEMO FIXTURE — NOT ACQUISITION                                                                                            
┌ Sources / views ───┐┌ Log viewport ──────────────────────────────────────────────────────────────────────────────────────────────────────┐
│● API fixture       ││time          level  event                                                                                          │
│  synthetic/live    ││12:00:01      INFO   fixture request 01 completed                                                                   │
│ › All events       ││12:00:02      INFO   fixture request 02 completed                                                                   │
│● Worker fixture    ││12:00:03      INFO   Unicode 東 京  café é request completed                                                          │
│  synthetic/static  ││12:00:04      INFO   fixture request 04 completed                                                                   │
│   Errors only      ││12:00:05      WARN   fixture request 05 completed                                                                   │
│                    ││12:00:06      INFO   fixture request 06 completed                                                                   │
┌ Bookmarks / notes · this view ───────────────────────────────────────────────────────────────────────────────────────────────────────────┐
│ 1 / 128 bookmarks ·                                                                                                                      │
│ Record api:16                                                                                                                            │
│                                                                                                                                          │
│                                                                                                                                          │
│                                                                                                                                          │
│                                                                                                                                          │
│                                                                                                                                          │
│                                                                                                                                          │
│                                                                                                                                          │
│                                                                                                                                          │
│                                                                                                                                          │
│                                                                                                                                          │
│                                                                                                                                          │
│                                                                                                                                          │
│                                                                                                                                          │
│                                                                                                                                          │
│ Note (1024 bytes)                                                                                                                        │
│ checked with the on-call rotation; correlates with the 12:00 deploy                                                                      │
│ [ Save note ]                                                                                                                            │
│↑/↓ select                                                                                                                                │
└──────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────┘
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
└────────────────────┘└────────────────────────────────────────────────────────────────────────────────────────────────────────────────────┘
                       FOLLOW | 1-16/16 | ? help                                                                                            

===== bookmarks @ 100x30 focus=Bookmarks surface=Some(Rect { x: 1, y: 5, width: 98, height: 20 }) dialog_scroll=0 help_scroll=0 scroll_hitbox=None =====
lvu log workspace DEMO FIXTURE — NOT ACQUISITION                                                    
┌ Sources / views ───┐┌ Log viewport ──────────────────────────────────────────────────────────────┐
│● API fixture       ││time          level  event                                                  │
│  synthetic/live    ││12:00:01      INFO   fixture request 01 completed                           │
┌ Bookmarks / notes · this view ───────────────────────────────────────────────────────────────────┐
│ 1 / 128 bookmarks ·                                                                              │
│ Record api:16                                                                                    │
│                                                                                                  │
│                                                                                                  │
│                                                                                                  │
│                                                                                                  │
│                                                                                                  │
│                                                                                                  │
│                                                                                                  │
│                                                                                                  │
│                                                                                                  │
│                                                                                                  │
│                                                                                                  │
│                                                                                                  │
│                                                                                                  │
│                                                                                                  │
│ Note (1024 bytes)                                                                                │
│ checked with the on-call rotation; correlates with the 12:00 deploy                              │
│ [ Save note ]                                                                                    │
│↑/↓ select                                                                                        │
└──────────────────────────────────────────────────────────────────────────────────────────────────┘
│                    ││                                                                            │
│                    ││                                                                            │
└────────────────────┘└────────────────────────────────────────────────────────────────────────────┘
                       FOLLOW | 1-16/16 | ? help                                                    

===== bookmarks @ 80x24 focus=Bookmarks surface=Some(Rect { x: 1, y: 2, width: 78, height: 20 }) dialog_scroll=0 help_scroll=0 scroll_hitbox=None =====
lvu log workspace DEMO FIXTURE — NOT ACQUISITION                                
┌ Bookmarks / notes · this view ───────────────────────────────────────────────┐
│ 1 / 128 bookmarks ·                                                          │
│ Record api:16                                                                │
│                                                                              │
│                                                                              │
│                                                                              │
│                                                                              │
│                                                                              │
│                                                                              │
│                                                                              │
│                                                                              │
│                                                                              │
│                                                                              │
│                                                                              │
│                                                                              │
│                                                                              │
│                                                                              │
│ Note (1024 bytes)                                                            │
│ checked with the on-call rotation; correlates with the 12:00 deploy          │
│ [ Save note ]                                                                │
│↑/↓ select                                                                    │
└──────────────────────────────────────────────────────────────────────────────┘
                       FOLLOW | 1-16/16 | ? help                                

===== bookmarks @ 54x16 focus=Bookmarks surface=Some(Rect { x: 1, y: 1, width: 52, height: 14 }) dialog_scroll=0 help_scroll=0 scroll_hitbox=None =====
┌ Bookmarks / notes · this view ─────────────────────┐
│ 1 / 128 bookmarks ·                                │
│ Record api:16                                      │
│                                                    │
│                                                    │
│                                                    │
│                                                    │
│                                                    │
│                                                    │
│                                                    │
│                                                    │
│ Note (1024 bytes)                                  │
│ n-call rotation; correlates with the 12:00 deployh │
│ [ Save note ]                                      │
│↑/↓ select                                          │
└────────────────────────────────────────────────────┘

===== views-prompt @ 140x40 focus=ViewDialog surface=Some(Rect { x: 18, y: 16, width: 104, height: 8 }) dialog_scroll=0 help_scroll=0 scroll_hitbox=None =====
lvu log workspace DEMO FIXTURE — NOT ACQUISITION                                                                                            
┌ Sources / views ───┐┌ Log viewport ──────────────────────────────────────────────────────────────────────────────────────────────────────┐
│● API fixture       ││time          level  event                                                                                          │
│  synthetic/live    ││12:00:01      INFO   fixture request 01 completed                                                                   │
│ › All events       ││12:00:02      INFO   fixture request 02 completed                                                                   │
│● Worker fixture    ││12:00:03      INFO   Unicode 東 京  café é request completed                                                          │
│  synthetic/static  ││12:00:04      INFO   fixture request 04 completed                                                                   │
│   Errors only      ││12:00:05      WARN   fixture request 05 completed                                                                   │
│                    ││12:00:06      INFO   fixture request 06 completed                                                                   │
│                    ││12:00:07      INFO   fixture request 07 completed                                                                   │
│                    ││12:00:08      INFO   fixture request 08 completed                                                                   │
│                    ││12:00:09      INFO   fixture request 09 completed                                                                   │
│                    ││12:00:10      WARN   fixture request 10 completed                                                                   │
│                    ││12:00:11      INFO   fixture request 11 completed                                                                   │
│                    ││12:00:12      INFO   fixture request 12 completed                                                                   │
│                ┌ Source view ───────────────────────────────────────────────────────────────────────────────────────────┐                │
│                │ Mode: CLONE SETTINGS                                                                                   │                │
│                │                                                                                                        │                │
│                │ Name: Copy of All events                                                                               │                │
│                │                                                                                                        │                │
│                │ Name the view. Creating, cloning, and renaming preserve the source capture.                            │                │
│                │[ New blank ] [ Clone ] [ Rename ] [ Sources ] [ Apply ]                                                │                │
│                │                                                                                                        │                │
│                │                                                                                                        │                │
│                └────────────────────────────────────────────────────────────────────────────────────────────────────────┘                │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
└────────────────────┘└────────────────────────────────────────────────────────────────────────────────────────────────────────────────────┘
                       FOLLOW | 1-16/16 | ? help                                                                                            

===== views-prompt @ 100x30 focus=ViewDialog surface=Some(Rect { x: 13, y: 11, width: 74, height: 8 }) dialog_scroll=0 help_scroll=0 scroll_hitbox=None =====
lvu log workspace DEMO FIXTURE — NOT ACQUISITION                                                    
┌ Sources / views ───┐┌ Log viewport ──────────────────────────────────────────────────────────────┐
│● API fixture       ││time          level  event                                                  │
│  synthetic/live    ││12:00:01      INFO   fixture request 01 completed                           │
│ › All events       ││12:00:02      INFO   fixture request 02 completed                           │
│● Worker fixture    ││12:00:03      INFO   Unicode 東 京  café é request completed                  │
│  synthetic/static  ││12:00:04      INFO   fixture request 04 completed                           │
│   Errors only      ││12:00:05      WARN   fixture request 05 completed                           │
│                    ││12:00:06      INFO   fixture request 06 completed                           │
│                    ││12:00:07      INFO   fixture request 07 completed                           │
│           ┌ Source view ─────────────────────────────────────────────────────────────┐           │
│           │ Mode: CLONE SETTINGS                                                     │           │
│           │                                                                          │           │
│           │ Name: Copy of All events                                                 │           │
│           │                                                                          │           │
│           │ Name the view. Creating, cloning, and renaming preserve the source       │           │
│           │[ New blank ] [ Clone ] [ Rename ] [ Sources ] [ Apply ]                  │           │
│           │                                                                          │           │
│           │                                                                          │           │
│           └──────────────────────────────────────────────────────────────────────────┘           │
│                    ││                                                                            │
│                    ││                                                                            │
│                    ││                                                                            │
│                    ││                                                                            │
│                    ││                                                                            │
│                    ││                                                                            │
│                    ││                                                                            │
│                    ││                                                                            │
└────────────────────┘└────────────────────────────────────────────────────────────────────────────┘
                       FOLLOW | 1-16/16 | ? help                                                    

===== views-prompt @ 80x24 focus=ViewDialog surface=Some(Rect { x: 11, y: 8, width: 58, height: 8 }) dialog_scroll=0 help_scroll=0 scroll_hitbox=None =====
lvu log workspace DEMO FIXTURE — NOT ACQUISITION                                
┌ Sources / views ───┐┌ Log viewport ──────────────────────────────────────────┐
│● API fixture       ││time          level  event                              │
│  synthetic/live    ││12:00:01      INFO   fixture request 01 completed       │
│ › All events       ││12:00:02      INFO   fixture request 02 completed       │
│● Worker fixture    ││12:00:03      INFO   Unicode 東 京  café é request complet│
│  synthetic/static  ││12:00:04      INFO   fixture request 04 completed       │
│   Errors┌ Source view ─────────────────────────────────────────────┐ed       │
│         │ Mode: CLONE SETTINGS                                     │ed       │
│         │                                                          │ed       │
│         │ Name: Copy of All events                                 │ed       │
│         │                                                          │ed       │
│         │ Name the view. Creating, cloning, and renaming preserve  │ed       │
│         │[ New blank ]a[ Clone ] [ Rename ] [ Sources ] [ Apply ]  │ed       │
│         │                                                          │ed       │
│         │                                                          │ed       │
│         └──────────────────────────────────────────────────────────┘ed       │
│                    ││12:00:15      WARN   fixture request 15 completed       │
│                    ││12:00:16      INFO   fixture request 16 completed       │
│                    ││                                                        │
│                    ││                                                        │
│                    ││                                                        │
└────────────────────┘└────────────────────────────────────────────────────────┘
                       FOLLOW | 1-16/16 | ? help                                

===== views-prompt @ 54x16 focus=ViewDialog surface=Some(Rect { x: 7, y: 4, width: 39, height: 8 }) dialog_scroll=0 help_scroll=0 scroll_hitbox=None =====
lvu log workspace DEMO FIXTURE — NOT ACQUISITION      
┌ Sources / views ───┐┌ Log viewport ────────────────┐
│● API fixture       ││time          level  event    │
│  syn┌ Source view ──────────────────────────┐ture r│
│ › Al│ Mode: CLONE SETTINGS                  │ture r│
│● Wor│                                       │ture r│
│  syn│ Name: Copy of All events              │ture r│
│   Er│                                       │ture r│
│     │ Name the view. Creating, cloning, and │ture r│
│     │[ New blank ]s[ Clone ]s[ Rename ]ure. │ture r│
│     │[ Sources ] [ Apply ]                  │ture r│
│     │                                       │ture r│
│     └───────────────────────────────────────┘ture r│
│                    ││12:00:16      INFO   fixture r│
└────────────────────┘└──────────────────────────────┘
                       FOLLOW | 6-16/16 | ? help      

===== views-sources @ 140x40 focus=ViewDialog surface=Some(Rect { x: 5, y: 10, width: 129, height: 20 }) dialog_scroll=0 help_scroll=0 scroll_hitbox=None =====
lvu log workspace DEMO FIXTURE — NOT ACQUISITION                                                                                            
┌ Sources / views ───┐┌ Log viewport ──────────────────────────────────────────────────────────────────────────────────────────────────────┐
│● API fixture       ││time          level  event                                                                                          │
│  synthetic/live    ││12:00:01      INFO   fixture request 01 completed                                                                   │
│ › All events       ││12:00:02      INFO   fixture request 02 completed                                                                   │
│● Worker fixture    ││12:00:03      INFO   Unicode 東 京  café é request completed                                                          │
│  synthetic/static  ││12:00:04      INFO   fixture request 04 completed                                                                   │
│   Errors only      ││12:00:05      WARN   fixture request 05 completed                                                                   │
│                    ││12:00:06      INFO   fixture request 06 completed                                                                   │
│   ┌ View sources · explicit source order ───────────────────────────────────────────────────────────────────────────────────────────┐    │
│   │ Order: source position, then record sequence (not clock order).                                                                 │    │
│   │ The owning source remains included; captures are shared.                                                                        │    │
│   │ [x]  1 API fixture                                                                                                              │    │
│   │ [ ]    Worker fixture                                                                                                           │    │
│   │                                                                                                                                 │    │
│   │                                                                                                                                 │    │
│   │                                                                                                                                 │    │
│   │                                                                                                                                 │    │
│   │                                                                                                                                 │    │
│   │                                                                                                                                 │    │
│   │                                                                                                                                 │    │
│   │                                                                                                                                 │    │
│   │                                                                                                                                 │    │
│   │                                                                                                                                 │    │
│   │                                                                                                                                 │    │
│   │                                                                                                                                 │    │
│   │                                                                                                                                 │    │
│   │[ New blank ] [ Clone ] [ Rename ] [ Sources ] [ Apply membership ]                                                              │    │
│   │                                                                                                                                 │    │
│   │                                                                                                                                 │    │
│   └─────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────┘    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
└────────────────────┘└────────────────────────────────────────────────────────────────────────────────────────────────────────────────────┘
                       FOLLOW | 1-16/16 | ? help                                                                                            

===== views-sources @ 100x30 focus=ViewDialog surface=Some(Rect { x: 4, y: 5, width: 92, height: 20 }) dialog_scroll=0 help_scroll=0 scroll_hitbox=None =====
lvu log workspace DEMO FIXTURE — NOT ACQUISITION                                                    
┌ Sources / views ───┐┌ Log viewport ──────────────────────────────────────────────────────────────┐
│● API fixture       ││time          level  event                                                  │
│  synthetic/live    ││12:00:01      INFO   fixture request 01 completed                           │
│ ›┌ View sources · explicit source order ──────────────────────────────────────────────────────┐  │
│● │ Order: source position, then record sequence (not clock order).                            │  │
│  │ The owning source remains included; captures are shared.                                   │  │
│  │ [x]  1 API fixture                                                                         │  │
│  │ [ ]    Worker fixture                                                                      │  │
│  │                                                                                            │  │
│  │                                                                                            │  │
│  │                                                                                            │  │
│  │                                                                                            │  │
│  │                                                                                            │  │
│  │                                                                                            │  │
│  │                                                                                            │  │
│  │                                                                                            │  │
│  │                                                                                            │  │
│  │                                                                                            │  │
│  │                                                                                            │  │
│  │                                                                                            │  │
│  │                                                                                            │  │
│  │[ New blank ] [ Clone ] [ Rename ] [ Sources ] [ Apply membership ]                         │  │
│  │                                                                                            │  │
│  │                                                                                            │  │
│  └────────────────────────────────────────────────────────────────────────────────────────────┘  │
│                    ││                                                                            │
│                    ││                                                                            │
└────────────────────┘└────────────────────────────────────────────────────────────────────────────┘
                       FOLLOW | 1-16/16 | ? help                                                    

===== views-sources @ 80x24 focus=ViewDialog surface=Some(Rect { x: 3, y: 2, width: 73, height: 20 }) dialog_scroll=0 help_scroll=0 scroll_hitbox=None =====
lvu log workspace DEMO FIXTURE — NOT ACQUISITION                                
┌ ┌ View sources · explicit source order ───────────────────────────────────┐──┐
│●│ Order: source position, then record sequence (not clock order).         │  │
│ │ The owning source remains included; captures are shared.                │  │
│ │ [x]  1 API fixture                                                      │  │
│●│ [ ]    Worker fixture                                                   │et│
│ │                                                                         │  │
│ │                                                                         │  │
│ │                                                                         │  │
│ │                                                                         │  │
│ │                                                                         │  │
│ │                                                                         │  │
│ │                                                                         │  │
│ │                                                                         │  │
│ │                                                                         │  │
│ │                                                                         │  │
│ │                                                                         │  │
│ │                                                                         │  │
│ │                                                                         │  │
│ │[ New blank ] [ Clone ] [ Rename ] [ Sources ] [ Apply membership ]      │  │
│ │                                                                         │  │
│ │                                                                         │  │
└─└─────────────────────────────────────────────────────────────────────────┘──┘
                       FOLLOW | 1-16/16 | ? help                                

===== views-sources @ 54x16 focus=ViewDialog surface=Some(Rect { x: 3, y: 1, width: 48, height: 14 }) dialog_scroll=0 help_scroll=0 scroll_hitbox=None =====
lv┌ View sources · explicit source order ──────────┐  
┌ │ Order: source position, then record sequence ( │─┐
│●│ The owning source remains included; captures a │ │
│ │ [x]  1 API fixture                             │r│
│ │ [ ]    Worker fixture                          │r│
│●│                                                │r│
│ │                                                │r│
│ │                                                │r│
│ │                                                │r│
│ │                                                │r│
│ │                                                │r│
│ │                                                │r│
│ │[ New blank ] [ Clone ] [ Rename ] [ Sources ]  │r│
│ │[ Apply membership ]                            │r│
└─│                                                │─┘
  └────────────────────────────────────────────────┘  

===== source-manual @ 140x40 focus=SourceDialog surface=Some(Rect { x: 1, y: 9, width: 138, height: 22 }) dialog_scroll=0 help_scroll=0 scroll_hitbox=None =====
lvu log workspace DEMO FIXTURE — NOT ACQUISITION                                                                                            
┌ Sources / views ───┐┌ Log viewport ──────────────────────────────────────────────────────────────────────────────────────────────────────┐
│● API fixture       ││time          level  event                                                                                          │
│  synthetic/live    ││12:00:01      INFO   fixture request 01 completed                                                                   │
│ › All events       ││12:00:02      INFO   fixture request 02 completed                                                                   │
│● Worker fixture    ││12:00:03      INFO   Unicode 東 京  café é request completed                                                          │
│  synthetic/static  ││12:00:04      INFO   fixture request 04 completed                                                                   │
│   Errors only      ││12:00:05      WARN   fixture request 05 completed                                                                   │
┌ Add source ──────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────┐
│ FILE PATH                                                                                                                                │
│                                                                                                                                          │
│                                                                                                                                          │
│                                                                                                                                          │
│                                                                                                                                          │
│                                                                                                                                          │
│                                                                                                                                          │
│                                                                                                                                          │
│                                                                                                                                          │
│                                                                                                                                          │
│                                                                                                                                          │
│                                                                                                                                          │
│                                                                                                                                          │
│                                                                                                                                          │
│                                                                                                                                          │
│                                                                                                                                          │
│ ┌ State ───────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────┐ │
│ │Ready: provide a file path or command. Capture starts only after submission.                                                          │ │
│ └──────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────┘ │
│                                                                                                                                          │
│ [ Manual ] [ Discover ] [ 🧠  ] [ File ] [ Command ]                                                                                      │
│                                                                                                                                          │
└──────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────┘
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
└────────────────────┘└────────────────────────────────────────────────────────────────────────────────────────────────────────────────────┘
                       FOLLOW | 1-16/16 | ? help                                                                                            

===== source-manual @ 100x30 focus=SourceDialog surface=Some(Rect { x: 1, y: 4, width: 98, height: 22 }) dialog_scroll=0 help_scroll=0 scroll_hitbox=None =====
lvu log workspace DEMO FIXTURE — NOT ACQUISITION                                                    
┌ Sources / views ───┐┌ Log viewport ──────────────────────────────────────────────────────────────┐
│● API fixture       ││time          level  event                                                  │
┌ Add source ──────────────────────────────────────────────────────────────────────────────────────┐
│ FILE PATH                                                                                        │
│                                                                                                  │
│                                                                                                  │
│                                                                                                  │
│                                                                                                  │
│                                                                                                  │
│                                                                                                  │
│                                                                                                  │
│                                                                                                  │
│                                                                                                  │
│                                                                                                  │
│                                                                                                  │
│                                                                                                  │
│                                                                                                  │
│                                                                                                  │
│                                                                                                  │
│ ┌ State ───────────────────────────────────────────────────────────────────────────────────────┐ │
│ │Ready: provide a file path or command. Capture starts only after submission.                  │ │
│ └──────────────────────────────────────────────────────────────────────────────────────────────┘ │
│                                                                                                  │
│ [ Manual ] [ Discover ] [ 🧠  ] [ File ] [ Command ]                                              │
│                                                                                                  │
└──────────────────────────────────────────────────────────────────────────────────────────────────┘
│                    ││                                                                            │
└────────────────────┘└────────────────────────────────────────────────────────────────────────────┘
                       FOLLOW | 1-16/16 | ? help                                                    

===== source-manual @ 80x24 focus=SourceDialog surface=Some(Rect { x: 1, y: 1, width: 78, height: 22 }) dialog_scroll=0 help_scroll=0 scroll_hitbox=None =====
┌ Add source ──────────────────────────────────────────────────────────────────┐
│ FILE PATH                                                                    │
│                                                                              │
│                                                                              │
│                                                                              │
│                                                                              │
│                                                                              │
│                                                                              │
│                                                                              │
│                                                                              │
│                                                                              │
│                                                                              │
│                                                                              │
│                                                                              │
│                                                                              │
│                                                                              │
│                                                                              │
│ ┌ State ───────────────────────────────────────────────────────────────────┐ │
│ │Ready: provide a file path or command. Capture starts only after          │ │
│ └──────────────────────────────────────────────────────────────────────────┘ │
│                                                                              │
│ [ Manual ] [ Discover ] [ 🧠  ] [ File ] [ Command ]                          │
│                                                                              │
└──────────────────────────────────────────────────────────────────────────────┘

===== source-manual @ 54x16 focus=SourceDialog surface=Some(Rect { x: 1, y: 1, width: 52, height: 14 }) dialog_scroll=0 help_scroll=0 scroll_hitbox=None =====
┌ Add source ────────────────────────────────────────┐
│ FILE PATH                                          │
│                                                    │
│                                                    │
│                                                    │
│                                                    │
│                                                    │
│                                                    │
│                                                    │
│ ┌ State ─────────────────────────────────────────┐ │
│ │Ready: provide a file path or command. Capture  │ │
│ └────────────────────────────────────────────────┘ │
│                                                    │
│ [ Manual ] [ Discover ] [ 🧠  ] [ File ]            │
│ [ Command ]                                        │
└────────────────────────────────────────────────────┘

===== source-discovery @ 140x40 focus=SourceDialog surface=Some(Rect { x: 1, y: 9, width: 138, height: 22 }) dialog_scroll=0 help_scroll=0 scroll_hitbox=Some(Rect { x: 2, y: 23, width: 136, height: 5 }) =====
lvu log workspace DEMO FIXTURE — NOT ACQUISITION                                                                                            
┌ Sources / views ───┐┌ Log viewport ──────────────────────────────────────────────────────────────────────────────────────────────────────┐
│● API fixture       ││time          level  event                                                                                          │
│  synthetic/live    ││12:00:01      INFO   fixture request 01 completed                                                                   │
│ › All events       ││12:00:02      INFO   fixture request 02 completed                                                                   │
│● Worker fixture    ││12:00:03      INFO   Unicode 東 京  café é request completed                                                          │
│  synthetic/static  ││12:00:04      INFO   fixture request 04 completed                                                                   │
│   Errors only      ││12:00:05      WARN   fixture request 05 completed                                                                   │
┌ Discover sources — selection never auto-starts ──────────────────────────────────────────────────────────────────────────────────────────┐
│ Search                                                                                                                                   │
│ 0/0 matches · selection never starts capture                                                                                             │
│   No matching candidates.                                                                                                                │
│                                                                                                                                          │
│                                                                                                                                          │
│                                                                                                                                          │
│                                                                                                                                          │
│                                                                                                                                          │
│                                                                                                                                          │
│                                                                                                                                          │
│                                                                                                                                          │
│                                                                                                                                          │
│                                                                                                                                          │
│                                                                                                                                          │
│ ┌ Diagnostics ─────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────┐ │
│ │No candidate selected.                                                                                                                │ │
│ │UPDATING: scanning bounded local providers…                                                                                           │ │
│ │                                                                                                                                      │ │
│ └──────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────┘ │
│                                                                                                                                          │
│ [ Manual ] [ Discover ] [ 🧠  ] [ Refresh ]                                                                                               │
│                                                                                                                                          │
└──────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────┘
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
└────────────────────┘└────────────────────────────────────────────────────────────────────────────────────────────────────────────────────┘
                       FOLLOW | 1-16/16 | ? help                                                                                            

===== source-discovery @ 100x30 focus=SourceDialog surface=Some(Rect { x: 1, y: 4, width: 98, height: 22 }) dialog_scroll=0 help_scroll=0 scroll_hitbox=Some(Rect { x: 2, y: 18, width: 96, height: 5 }) =====
lvu log workspace DEMO FIXTURE — NOT ACQUISITION                                                    
┌ Sources / views ───┐┌ Log viewport ──────────────────────────────────────────────────────────────┐
│● API fixture       ││time          level  event                                                  │
┌ Discover sources — selection never auto-starts ──────────────────────────────────────────────────┐
│ Search                                                                                           │
│ 0/0 matches · selection never starts capture                                                     │
│   No matching candidates.                                                                        │
│                                                                                                  │
│                                                                                                  │
│                                                                                                  │
│                                                                                                  │
│                                                                                                  │
│                                                                                                  │
│                                                                                                  │
│                                                                                                  │
│                                                                                                  │
│                                                                                                  │
│                                                                                                  │
│ ┌ Diagnostics ─────────────────────────────────────────────────────────────────────────────────┐ │
│ │No candidate selected.                                                                        │ │
│ │UPDATING: scanning bounded local providers…                                                   │ │
│ │                                                                                              │ │
│ └──────────────────────────────────────────────────────────────────────────────────────────────┘ │
│                                                                                                  │
│ [ Manual ] [ Discover ] [ 🧠  ] [ Refresh ]                                                       │
│                                                                                                  │
└──────────────────────────────────────────────────────────────────────────────────────────────────┘
│                    ││                                                                            │
└────────────────────┘└────────────────────────────────────────────────────────────────────────────┘
                       FOLLOW | 1-16/16 | ? help                                                    

===== source-discovery @ 80x24 focus=SourceDialog surface=Some(Rect { x: 1, y: 1, width: 78, height: 22 }) dialog_scroll=0 help_scroll=0 scroll_hitbox=Some(Rect { x: 2, y: 15, width: 76, height: 5 }) =====
┌ Discover sources — selection never auto-starts ──────────────────────────────┐
│ Search                                                                       │
│ 0/0 matches · selection never starts capture                                 │
│   No matching candidates.                                                    │
│                                                                              │
│                                                                              │
│                                                                              │
│                                                                              │
│                                                                              │
│                                                                              │
│                                                                              │
│                                                                              │
│                                                                              │
│                                                                              │
│                                                                              │
│ ┌ Diagnostics ─────────────────────────────────────────────────────────────┐ │
│ │No candidate selected.                                                    │ │
│ │UPDATING: scanning bounded local providers…                               │ │
│ │                                                                          │ │
│ └──────────────────────────────────────────────────────────────────────────┘ │
│                                                                              │
│ [ Manual ] [ Discover ] [ 🧠  ] [ Refresh ]                                   │
│                                                                              │
└──────────────────────────────────────────────────────────────────────────────┘

===== source-discovery @ 54x16 focus=SourceDialog surface=Some(Rect { x: 1, y: 1, width: 52, height: 14 }) dialog_scroll=0 help_scroll=0 scroll_hitbox=Some(Rect { x: 2, y: 7, width: 50, height: 5 }) =====
┌ Discover sources — selection never auto-starts ────┐
│ Search                                             │
│ 0/0 matches · selection never starts capture       │
│   No matching candidates.                          │
│                                                    │
│                                                    │
│                                                    │
│ ┌ Diagnostics ───────────────────────────────────┐ │
│ │No candidate selected.                          │ │
│ │UPDATING: scanning bounded local providers…     │ │
│ │                                                │ │
│ └────────────────────────────────────────────────┘ │
│                                                    │
│ [ Manual ] [ Discover ] [ 🧠  ] [ Refresh ]         │
│                                                    │
└────────────────────────────────────────────────────┘

===== source-ai-proposal @ 140x40 focus=SourceDialog surface=Some(Rect { x: 1, y: 9, width: 138, height: 22 }) dialog_scroll=0 help_scroll=0 scroll_hitbox=None =====
lvu log workspace DEMO FIXTURE — NOT ACQUISITION                                                                                            
┌ Sources / views ───┐┌ Log viewport ──────────────────────────────────────────────────────────────────────────────────────────────────────┐
│● API fixture       ││time          level  event                                                                                          │
│  synthetic/live    ││12:00:01      INFO   fixture request 01 completed                                                                   │
│ › All events       ││12:00:02      INFO   fixture request 02 completed                                                                   │
│● Worker fixture    ││12:00:03      INFO   Unicode 東 京  café é request completed                                                          │
│  synthetic/static  ││12:00:04      INFO   fixture request 04 completed                                                                   │
│   Errors only      ││12:00:05      WARN   fixture request 05 completed                                                                   │
┌ Ask 🧠  for a source — preview never executes ────────────────────────────────────────────────────────────────────────────────────────────┐
│ Request                                                                                                                                  │
│ tail the nginx access log                                                                                                                │
│ ┌ State ───────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────┐ │
│ │Proposal: Review only — explicit confirmation starts this source                                                                      │ │
│ └──────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────┘ │
│ ┌ Preview ─────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────┐ │
│ │Name: nginx access log                                                                                                                │ │
│ │Kind: command                                                                                                                         │ │
│ │Launch: tail -F /var/log/nginx/access.log                                                                                             │ │
│ │Effective path/cwd: /var/log/nginx                                                                                                    │ │
│ │Restart: restart on exit with backoff                                                                                                 │ │
│ │Env: TZ=UTC                                                                                                                           │ │
│ │Env: LANG=C.UTF-8                                                                                                                     │ │
│ │Env: NGINX_LOG_FORMAT=combined                                                                                                        │ │
│ │Why: Follows the running access log without reading historical rotations. Capture keeps the original bytes; the view adds no filter.  │ │
│ │                                                                                                                                      │ │
│ │                                                                                                                                      │ │
│ └──────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────┘ │
│ Describe a source; review is required before capture starts.                                                                             │
│                                                                                                                                          │
│ [ Start reviewed ] [ Manual ] [ Discover ] [ 🧠  ]                                                                                        │
│                                                                                                                                          │
└──────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────┘
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
└────────────────────┘└────────────────────────────────────────────────────────────────────────────────────────────────────────────────────┘
                       FOLLOW | 1-16/16 | ? help                                                                                            

===== source-ai-proposal @ 100x30 focus=SourceDialog surface=Some(Rect { x: 1, y: 4, width: 98, height: 22 }) dialog_scroll=0 help_scroll=0 scroll_hitbox=None =====
lvu log workspace DEMO FIXTURE — NOT ACQUISITION                                                    
┌ Sources / views ───┐┌ Log viewport ──────────────────────────────────────────────────────────────┐
│● API fixture       ││time          level  event                                                  │
┌ Ask 🧠  for a source — preview never executes ────────────────────────────────────────────────────┐
│ Request                                                                                          │
│ tail the nginx access log                                                                        │
│ ┌ State ───────────────────────────────────────────────────────────────────────────────────────┐ │
│ │Proposal: Review only — explicit confirmation starts this source                              │ │
│ └──────────────────────────────────────────────────────────────────────────────────────────────┘ │
│ ┌ Preview ─────────────────────────────────────────────────────────────────────────────────────┐ │
│ │Name: nginx access log                                                                        │ │
│ │Kind: command                                                                                 │ │
│ │Launch: tail -F /var/log/nginx/access.log                                                     │ │
│ │Effective path/cwd: /var/log/nginx                                                            │ │
│ │Restart: restart on exit with backoff                                                         │ │
│ │Env: TZ=UTC                                                                                   │ │
│ │Env: LANG=C.UTF-8                                                                             │ │
│ │Env: NGINX_LOG_FORMAT=combined                                                                │ │
│ │Why: Follows the running access log without reading historical rotations. Capture keeps the   │ │
│ │original bytes; the view adds no filter.                                                      │ │
│ │                                                                                              │ │
│ └──────────────────────────────────────────────────────────────────────────────────────────────┘ │
│ Describe a source; review is required before capture starts.                                     │
│                                                                                                  │
│ [ Start reviewed ] [ Manual ] [ Discover ] [ 🧠  ]                                                │
│                                                                                                  │
└──────────────────────────────────────────────────────────────────────────────────────────────────┘
│                    ││                                                                            │
└────────────────────┘└────────────────────────────────────────────────────────────────────────────┘
                       FOLLOW | 1-16/16 | ? help                                                    

===== source-ai-proposal @ 80x24 focus=SourceDialog surface=Some(Rect { x: 1, y: 1, width: 78, height: 22 }) dialog_scroll=0 help_scroll=0 scroll_hitbox=None =====
┌ Ask 🧠  for a source — preview never executes ────────────────────────────────┐
│ Request                                                                      │
│ tail the nginx access log                                                    │
│ ┌ State ───────────────────────────────────────────────────────────────────┐ │
│ │Proposal: Review only — explicit confirmation starts this source          │ │
│ └──────────────────────────────────────────────────────────────────────────┘ │
│ ┌ Preview ─────────────────────────────────────────────────────────────────┐ │
│ │Name: nginx access log                                                    │ │
│ │Kind: command                                                             │ │
│ │Launch: tail -F /var/log/nginx/access.log                                 │ │
│ │Effective path/cwd: /var/log/nginx                                        │ │
│ │Restart: restart on exit with backoff                                     │ │
│ │Env: TZ=UTC                                                               │ │
│ │Env: LANG=C.UTF-8                                                         │ │
│ │Env: NGINX_LOG_FORMAT=combined                                            │ │
│ │Why: Follows the running access log without reading historical rotations. │ │
│ │Capture keeps the original bytes; the view adds no filter.                │ │
│ │                                                                          │ │
│ └──────────────────────────────────────────────────────────────────────────┘ │
│ Describe a source; review is required before capture starts.                 │
│                                                                              │
│ [ Start reviewed ] [ Manual ] [ Discover ] [ 🧠  ]                            │
│                                                                              │
└──────────────────────────────────────────────────────────────────────────────┘

===== source-ai-proposal @ 54x16 focus=SourceDialog surface=Some(Rect { x: 1, y: 1, width: 52, height: 14 }) dialog_scroll=0 help_scroll=0 scroll_hitbox=Some(Rect { x: 2, y: 6, width: 50, height: 5 }) =====
┌ Ask 🧠  for a source — preview never executes ──────┐
│ Request                                            │
│ tail the nginx access log                          │
│ ┌ State ─────────────────────────────────────────┐ │
│ │Proposal: Review only — explicit confirmation   │ │
│ └────────────────────────────────────────────────┘ │
│ ┌ Preview · lines 1–3 of 11 · ↑/↓ ───────────────┐ │
│ │Name: nginx access log                          │ │
│ │Kind: command                                   │ │
│ │Launch: tail -F /var/log/nginx/access.log       │ │
│ └────────────────────────────────────────────────┘ │
│ Describe a source; review is required before captu │
│                                                    │
│ [ Start reviewed ] [ Manual ] [ Discover ] [ 🧠  ]  │
│                                                    │
└────────────────────────────────────────────────────┘

===== ask-ai-proposal @ 140x40 focus=AskAi surface=Some(Rect { x: 5, y: 9, width: 129, height: 22 }) dialog_scroll=0 help_scroll=0 scroll_hitbox=Some(Rect { x: 7, y: 21, width: 125, height: 8 }) =====
lvu log workspace DEMO FIXTURE — NOT ACQUISITION                                                                                            
┌ Sources / views ───┐┌ Log viewport ──────────────────────────────────────────────────────────────────────────────────────────────────────┐
│● API fixture       ││time          level  event                                                                                          │
│  synthetic/live    ││12:00:01      INFO   fixture request 01 completed                                                                   │
│ › All events       ││12:00:02      INFO   fixture request 02 completed                                                                   │
│● Worker fixture    ││12:00:03      INFO   Unicode 東 京  café é request completed                                                          │
│  synthetic/static  ││12:00:04      INFO   fixture request 04 completed                                                                   │
│   Errors only      ││12:00:05      WARN   fixture request 05 completed                                                                   │
│   ┌ Ask 🧠  ─────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────┐    │
│   │ [ Kind: Filter ▾ ] [ Apply ]                                                                                                    │    │
│   │ ┌ Request ────────────────────────────────────────────────────────────────────────────────────────────────────────────────────┐ │    │
│   │ │only warnings from the api service                                                                                           │ │    │
│   │ │                                                                                                                             │ │    │
│   │ │                                                                                                                             │ │    │
│   │ │                                                                                                                             │ │    │
│   │ └─────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────┘ │    │
│   │ ┌ State ──────────────────────────────────────────────────────────────────────────────────────────────────────────────────────┐ │    │
│   │ │Proposal: Proposal ready — review before applying                                                                            │ │    │
│   │ │                                                                                                                             │ │    │
│   │ └─────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────┘ │    │
│   │ ┌ Proposal and activity ──────────────────────────────────────────────────────────────────────────────────────────────────────┐ │    │
│   │ │Agent: codex/gpt-5.6-sol · mode full-access · thinking medium                                                                │ │    │
│   │ │Submitted request: only warnings from the api service                                                                        │ │    │
│   │ │Proposal: col("level").eq(lit("WARN")).and(col("service").eq(lit("api")))                                                    │ │    │
│   │ │Explanation: Keeps WARN rows emitted by the api service. Other services and other levels stay excluded; the accepted filter  │ │    │
│   │ │is unchanged until you apply this.                                                                                           │ │    │
│   │ │                                                                                                                             │ │    │
│   │ │                                                                                                                             │ │    │
│   │ │                                                                                                                             │ │    │
│   │ └─────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────┘ │    │
│   │                                                                                                                                 │    │
│   └─────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────┘    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
└────────────────────┘└────────────────────────────────────────────────────────────────────────────────────────────────────────────────────┘
                       FOLLOW | 1-16/16 | ? help                                                                                            

===== ask-ai-proposal @ 100x30 focus=AskAi surface=Some(Rect { x: 4, y: 4, width: 92, height: 22 }) dialog_scroll=0 help_scroll=0 scroll_hitbox=Some(Rect { x: 6, y: 16, width: 88, height: 8 }) =====
lvu log workspace DEMO FIXTURE — NOT ACQUISITION                                                    
┌ Sources / views ───┐┌ Log viewport ──────────────────────────────────────────────────────────────┐
│● API fixture       ││time          level  event                                                  │
│  ┌ Ask 🧠  ────────────────────────────────────────────────────────────────────────────────────┐  │
│ ›│ [ Kind: Filter ▾ ] [ Apply ]                                                               │  │
│● │ ┌ Request ───────────────────────────────────────────────────────────────────────────────┐ │  │
│  │ │only warnings from the api service                                                      │ │  │
│  │ │                                                                                        │ │  │
│  │ │                                                                                        │ │  │
│  │ │                                                                                        │ │  │
│  │ └────────────────────────────────────────────────────────────────────────────────────────┘ │  │
│  │ ┌ State ─────────────────────────────────────────────────────────────────────────────────┐ │  │
│  │ │Proposal: Proposal ready — review before applying                                       │ │  │
│  │ │                                                                                        │ │  │
│  │ └────────────────────────────────────────────────────────────────────────────────────────┘ │  │
│  │ ┌ Proposal and activity ─────────────────────────────────────────────────────────────────┐ │  │
│  │ │Agent: codex/gpt-5.6-sol · mode full-access · thinking medium                           │ │  │
│  │ │Submitted request: only warnings from the api service                                   │ │  │
│  │ │Proposal: col("level").eq(lit("WARN")).and(col("service").eq(lit("api")))               │ │  │
│  │ │Explanation: Keeps WARN rows emitted by the api service. Other services and other levels│ │  │
│  │ │stay excluded; the accepted filter is unchanged until you apply this.                   │ │  │
│  │ │                                                                                        │ │  │
│  │ │                                                                                        │ │  │
│  │ │                                                                                        │ │  │
│  │ └────────────────────────────────────────────────────────────────────────────────────────┘ │  │
│  │                                                                                            │  │
│  └────────────────────────────────────────────────────────────────────────────────────────────┘  │
│                    ││                                                                            │
└────────────────────┘└────────────────────────────────────────────────────────────────────────────┘
                       FOLLOW | 1-16/16 | ? help                                                    

===== ask-ai-proposal @ 80x24 focus=AskAi surface=Some(Rect { x: 3, y: 1, width: 73, height: 22 }) dialog_scroll=0 help_scroll=0 scroll_hitbox=Some(Rect { x: 5, y: 13, width: 69, height: 8 }) =====
lv┌ Ask 🧠  ─────────────────────────────────────────────────────────────────┐   
┌ │ [ Kind: Filter ▾ ] [ Apply ]                                            │──┐
│●│ ┌ Request ────────────────────────────────────────────────────────────┐ │  │
│ │ │only warnings from the api service                                   │ │  │
│ │ │                                                                     │ │  │
│●│ │                                                                     │ │et│
│ │ │                                                                     │ │  │
│ │ └─────────────────────────────────────────────────────────────────────┘ │  │
│ │ ┌ State ──────────────────────────────────────────────────────────────┐ │  │
│ │ │Proposal: Proposal ready — review before applying                    │ │  │
│ │ │                                                                     │ │  │
│ │ └─────────────────────────────────────────────────────────────────────┘ │  │
│ │ ┌ Proposal and activity ──────────────────────────────────────────────┐ │  │
│ │ │Agent: codex/gpt-5.6-sol · mode full-access · thinking medium        │ │  │
│ │ │Submitted request: only warnings from the api service                │ │  │
│ │ │Proposal:                                                            │ │  │
│ │ │col("level").eq(lit("WARN")).and(col("service").eq(lit("api")))      │ │  │
│ │ │Explanation: Keeps WARN rows emitted by the api service. Other       │ │  │
│ │ │services and other levels stay excluded; the accepted filter is      │ │  │
│ │ │unchanged until you apply this.                                      │ │  │
│ │ │                                                                     │ │  │
│ │ └─────────────────────────────────────────────────────────────────────┘ │  │
└─│                                                                         │──┘
  └─────────────────────────────────────────────────────────────────────────┘   

===== ask-ai-proposal @ 54x16 focus=AskAi surface=Some(Rect { x: 3, y: 1, width: 48, height: 14 }) dialog_scroll=0 help_scroll=0 scroll_hitbox=Some(Rect { x: 5, y: 10, width: 44, height: 3 }) =====
lv┌ Ask 🧠  ────────────────────────────────────────┐  
┌ │ [ Kind: Filter ▾ ]                    [ More ] │─┐
│●│ [ Apply ]                                      │ │
│ │ ┌ Request ───────────────────────────────────┐ │r│
│ │ │only warnings from the api service          │ │r│
│●│ └────────────────────────────────────────────┘ │r│
│ │ ┌ State ─────────────────────────────────────┐ │r│
│ │ │Proposal: Proposal ready — review before    │ │r│
│ │ └────────────────────────────────────────────┘ │r│
│ │ ┌ Proposal and activity ─────────────────────┐ │r│
│ │ │Agent: codex/gpt-5.6-sol · mode full-access │ │r│
│ │ │· thinking medium                           │ │r│
│ │ │Submitted request: only warnings from the   │ │r│
│ │ └────────────────────────────────────────────┘ │r│
└─│                                                │─┘
  └────────────────────────────────────────────────┘  

===== investigation @ 140x40 focus=Investigation surface=Some(Rect { x: 5, y: 9, width: 129, height: 22 }) dialog_scroll=0 help_scroll=0 scroll_hitbox=Some(Rect { x: 7, y: 20, width: 125, height: 9 }) =====
lvu log workspace DEMO FIXTURE — NOT ACQUISITION                                                                                            
┌ Sources / views ───┐┌ Log viewport ──────────────────────────────────────────────────────────────────────────────────────────────────────┐
│● API fixture       ││time          level  event                                                                                          │
│  synthetic/live    ││12:00:01      INFO   fixture request 01 completed                                                                   │
│ › All events       ││12:00:02      INFO   fixture request 02 completed                                                                   │
│● Worker fixture    ││12:00:03      INFO   Unicode 東 京  café é request completed                                                          │
│  synthetic/static  ││12:00:04      INFO   fixture request 04 completed                                                                   │
│   Errors only      ││12:00:05      WARN   fixture request 05 completed                                                                   │
│   ┌ Investigation 🧠  ───────────────────────────────────────────────────────────────────────────────────────────────────────────────┐    │
│   │ [ Send ]                                                                                                                        │    │
│   │                                                                                                                                 │    │
│   │ ┌ Question or follow-up ──────────────────────────────────────────────────────────────────────────────────────────────────────┐ │    │
│   │ │why did request latency spike at 12:00?                                                                                      │ │    │
│   │ │                                                                                                                             │ │    │
│   │ │                                                                                                                             │ │    │
│   │ └─────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────┘ │    │
│   │ ┌ State ──────────────────────────────────────────────────────────────────────────────────────────────────────────────────────┐ │    │
│   │ │Ready: enter a question for a new fixed snapshot                                                                             │ │    │
│   │ └─────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────┘ │    │
│   │ ┌ Activity and saved investigations ──────────────────────────────────────────────────────────────────────────────────────────┐ │    │
│   │ │Conversation:                                                                                                                │ │    │
│   │ │activity 0: inspected the WARN burst and the worker restart window                                                           │ │    │
│   │ │activity 1: inspected the WARN burst and the worker restart window                                                           │ │    │
│   │ │activity 2: inspected the WARN burst and the worker restart window                                                           │ │    │
│   │ │activity 3: inspected the WARN burst and the worker restart window                                                           │ │    │
│   │ │activity 4: inspected the WARN burst and the worker restart window                                                           │ │    │
│   │ │activity 5: inspected the WARN burst and the worker restart window                                                           │ │    │
│   │ │                                                                                                                             │ │    │
│   │ │                                                                                                                             │ │    │
│   │ └─────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────┘ │    │
│   │                                                                                                                                 │    │
│   └─────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────┘    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
└────────────────────┘└────────────────────────────────────────────────────────────────────────────────────────────────────────────────────┘
                       FOLLOW | 1-16/16 | ? help                                                                                            

===== investigation @ 100x30 focus=Investigation surface=Some(Rect { x: 4, y: 4, width: 92, height: 22 }) dialog_scroll=0 help_scroll=0 scroll_hitbox=Some(Rect { x: 6, y: 15, width: 88, height: 9 }) =====
lvu log workspace DEMO FIXTURE — NOT ACQUISITION                                                    
┌ Sources / views ───┐┌ Log viewport ──────────────────────────────────────────────────────────────┐
│● API fixture       ││time          level  event                                                  │
│  ┌ Investigation 🧠  ──────────────────────────────────────────────────────────────────────────┐  │
│ ›│ [ Send ]                                                                                   │  │
│● │                                                                                            │  │
│  │ ┌ Question or follow-up ─────────────────────────────────────────────────────────────────┐ │  │
│  │ │why did request latency spike at 12:00?                                                 │ │  │
│  │ │                                                                                        │ │  │
│  │ │                                                                                        │ │  │
│  │ └────────────────────────────────────────────────────────────────────────────────────────┘ │  │
│  │ ┌ State ─────────────────────────────────────────────────────────────────────────────────┐ │  │
│  │ │Ready: enter a question for a new fixed snapshot                                        │ │  │
│  │ └────────────────────────────────────────────────────────────────────────────────────────┘ │  │
│  │ ┌ Activity and saved investigations ─────────────────────────────────────────────────────┐ │  │
│  │ │Conversation:                                                                           │ │  │
│  │ │activity 0: inspected the WARN burst and the worker restart window                      │ │  │
│  │ │activity 1: inspected the WARN burst and the worker restart window                      │ │  │
│  │ │activity 2: inspected the WARN burst and the worker restart window                      │ │  │
│  │ │activity 3: inspected the WARN burst and the worker restart window                      │ │  │
│  │ │activity 4: inspected the WARN burst and the worker restart window                      │ │  │
│  │ │activity 5: inspected the WARN burst and the worker restart window                      │ │  │
│  │ │                                                                                        │ │  │
│  │ │                                                                                        │ │  │
│  │ └────────────────────────────────────────────────────────────────────────────────────────┘ │  │
│  │                                                                                            │  │
│  └────────────────────────────────────────────────────────────────────────────────────────────┘  │
│                    ││                                                                            │
└────────────────────┘└────────────────────────────────────────────────────────────────────────────┘
                       FOLLOW | 1-16/16 | ? help                                                    

===== investigation @ 80x24 focus=Investigation surface=Some(Rect { x: 3, y: 1, width: 73, height: 22 }) dialog_scroll=0 help_scroll=0 scroll_hitbox=Some(Rect { x: 5, y: 12, width: 69, height: 9 }) =====
lv┌ Investigation 🧠  ───────────────────────────────────────────────────────┐   
┌ │ [ Send ]                                                                │──┐
│●│                                                                         │  │
│ │ ┌ Question or follow-up ──────────────────────────────────────────────┐ │  │
│ │ │why did request latency spike at 12:00?                              │ │  │
│●│ │                                                                     │ │et│
│ │ │                                                                     │ │  │
│ │ └─────────────────────────────────────────────────────────────────────┘ │  │
│ │ ┌ State ──────────────────────────────────────────────────────────────┐ │  │
│ │ │Ready: enter a question for a new fixed snapshot                     │ │  │
│ │ └─────────────────────────────────────────────────────────────────────┘ │  │
│ │ ┌ Activity and saved investigations ──────────────────────────────────┐ │  │
│ │ │Conversation:                                                        │ │  │
│ │ │activity 0: inspected the WARN burst and the worker restart window   │ │  │
│ │ │activity 1: inspected the WARN burst and the worker restart window   │ │  │
│ │ │activity 2: inspected the WARN burst and the worker restart window   │ │  │
│ │ │activity 3: inspected the WARN burst and the worker restart window   │ │  │
│ │ │activity 4: inspected the WARN burst and the worker restart window   │ │  │
│ │ │activity 5: inspected the WARN burst and the worker restart window   │ │  │
│ │ │                                                                     │ │  │
│ │ │                                                                     │ │  │
│ │ └─────────────────────────────────────────────────────────────────────┘ │  │
└─│                                                                         │──┘
  └─────────────────────────────────────────────────────────────────────────┘   

===== investigation @ 54x16 focus=Investigation surface=Some(Rect { x: 3, y: 1, width: 48, height: 14 }) dialog_scroll=0 help_scroll=0 scroll_hitbox=Some(Rect { x: 5, y: 12, width: 44, height: 1 }) =====
lv┌ Investigation 🧠  ──────────────────────────────┐  
┌ │ [ Send ] [ More ]                              │─┐
│●│                                                │ │
│ │ ┌ Question or follow-up ─────────────────────┐ │r│
│ │ │why did request latency spike at 12:00?     │ │r│
│●│ │                                            │ │r│
│ │ │                                            │ │r│
│ │ └────────────────────────────────────────────┘ │r│
│ │ ┌ State ─────────────────────────────────────┐ │r│
│ │ │Ready: enter a question for a new fixed     │ │r│
│ │ └────────────────────────────────────────────┘ │r│
│ │ ┌ Activity and saved investigations ─────────┐ │r│
│ │ │Conversation:                               │ │r│
│ │ └────────────────────────────────────────────┘ │r│
└─│                                                │─┘
  └────────────────────────────────────────────────┘  

===== command-enrichment @ 140x40 focus=CommandEnrichment surface=Some(Rect { x: 11, y: 9, width: 118, height: 22 }) dialog_scroll=0 help_scroll=0 scroll_hitbox=None =====
lvu log workspace DEMO FIXTURE — NOT ACQUISITION                                                                                            
┌ Sources / views ───┐┌ Log viewport ──────────────────────────────────────────────────────────────────────────────────────────────────────┐
│● API fixture       ││time          level  event                                                                                          │
│  synthetic/live    ││12:00:01      INFO   fixture request 01 completed                                                                   │
│ › All events       ││12:00:02      INFO   fixture request 02 completed                                                                   │
│● Worker fixture    ││12:00:03      INFO   Unicode 東 京  café é request completed                                                          │
│  synthetic/static  ││12:00:04      INFO   fixture request 04 completed                                                                   │
│   Errors only      ││12:00:05      WARN   fixture request 05 completed                                                                   │
│         ┌ External command · runs only when confirmed ─────────────────────────────────────────────────────────────────────────┐         │
│         │ Program: Executable path; no shell parsing                                                                           │         │
│         │ /usr/bin/jq                                                                                                          │         │
│         │ Arguments: 1 line(s) · One argument per line, e.g. --format then json                                                │         │
│         │ -r '.trace_id'                                                                                                       │         │
│         │ Working directory: Optional; defaults to this workspace directory                                                    │         │
│         │ /home/user/work                                                                                                      │         │
│         │ Environment: 1 line(s) · Optional KEY=value per line, e.g. LANG=C                                                    │         │
│         │ TZ=UTC                                                                                                               │         │
│         │ Applied command step: None · enrichment steps still apply                                                            │         │
│         │                                                                                                                      │         │
│         │ [ New line (Alt-N) ] [ Save ] [ Review ] [ Remove ]                                                                  │         │
│         │                                                                                                                      │         │
│         │ ┌ Status and review ───────────────────────────────────────────────────────────────────────────────────────────────┐ │         │
│         │ │Status: Unrun · Reviewed 16 sampled records; 3 produced no output and stay unenriched.                            │ │         │
│         │ │Saving or restoring never starts this command. New records remain pending until another explicit run.             │ │         │
│         │ │Results appear in Details as command.<field>; command.status shows Ready or Pending. Filters and field choices use│ │         │
│         │ │the enrichment steps above.                                                                                       │ │         │
│         │ │                                                                                                                  │ │         │
│         │ │                                                                                                                  │ │         │
│         │ │                                                                                                                  │ │         │
│         │ │                                                                                                                  │ │         │
│         │ └──────────────────────────────────────────────────────────────────────────────────────────────────────────────────┘ │         │
│         └──────────────────────────────────────────────────────────────────────────────────────────────────────────────────────┘         │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
└────────────────────┘└────────────────────────────────────────────────────────────────────────────────────────────────────────────────────┘
                       FOLLOW | 1-16/16 | ? help                                                                                            

===== command-enrichment @ 100x30 focus=CommandEnrichment surface=Some(Rect { x: 8, y: 4, width: 84, height: 22 }) dialog_scroll=0 help_scroll=0 scroll_hitbox=None =====
lvu log workspace DEMO FIXTURE — NOT ACQUISITION                                                    
┌ Sources / views ───┐┌ Log viewport ──────────────────────────────────────────────────────────────┐
│● API fixture       ││time          level  event                                                  │
│  synt┌ External command · runs only when confirmed ───────────────────────────────────────┐      │
│ › All│ Program: Executable path; no shell parsing                                         │      │
│● Work│ /usr/bin/jq                                                                        │      │
│  synt│ Arguments: 1 line(s) · One argument per line, e.g. --format then json              │      │
│   Err│ -r '.trace_id'                                                                     │      │
│      │ Working directory: Optional; defaults to this workspace directory                  │      │
│      │ /home/user/work                                                                    │      │
│      │ Environment: 1 line(s) · Optional KEY=value per line, e.g. LANG=C                  │      │
│      │ TZ=UTC                                                                             │      │
│      │ Applied command step: None · enrichment steps still apply                          │      │
│      │                                                                                    │      │
│      │ [ New line (Alt-N) ] [ Save ] [ Review ] [ Remove ]                                │      │
│      │                                                                                    │      │
│      │ ┌ Status and review ─────────────────────────────────────────────────────────────┐ │      │
│      │ │Status: Unrun · Reviewed 16 sampled records; 3 produced no output and stay      │ │      │
│      │ │unenriched.                                                                     │ │      │
│      │ │Saving or restoring never starts this command. New records remain pending until │ │      │
│      │ │another explicit run.                                                           │ │      │
│      │ │Results appear in Details as command.<field>; command.status shows Ready or     │ │      │
│      │ │Pending. Filters and field choices use the enrichment steps above.              │ │      │
│      │ │                                                                                │ │      │
│      │ │                                                                                │ │      │
│      │ └────────────────────────────────────────────────────────────────────────────────┘ │      │
│      └────────────────────────────────────────────────────────────────────────────────────┘      │
│                    ││                                                                            │
└────────────────────┘└────────────────────────────────────────────────────────────────────────────┘
                       FOLLOW | 1-16/16 | ? help                                                    

===== command-enrichment @ 80x24 focus=CommandEnrichment surface=Some(Rect { x: 7, y: 1, width: 66, height: 22 }) dialog_scroll=0 help_scroll=0 scroll_hitbox=None =====
lvu lo┌ External command · runs only when confirmed ─────────────────────┐      
┌ Sour│ Program: Executable path; no shell parsing                       │─────┐
│● API│ /usr/bin/jq                                                      │     │
│  syn│ Arguments: 1 line(s) · One argument per line, e.g. --format then │     │
│ › Al│ -r '.trace_id'                                                   │     │
│● Wor│ Working directory: Optional; defaults to this workspace director │mplet│
│  syn│ /home/user/work                                                  │     │
│   Er│ Environment: 1 line(s) · Optional KEY=value per line, e.g. LANG= │     │
│     │ TZ=UTC                                                           │     │
│     │ Applied command step: None · enrichment steps still apply        │     │
│     │                                                                  │     │
│     │ [ New line (Alt-N) ] [ Save ] [ Review ] [ Remove ]              │     │
│     │                                                                  │     │
│     │ ┌ Status and review ───────────────────────────────────────────┐ │     │
│     │ │Status: Unrun · Reviewed 16 sampled records; 3 produced no    │ │     │
│     │ │output and stay unenriched.                                   │ │     │
│     │ │Saving or restoring never starts this command. New records    │ │     │
│     │ │remain pending until another explicit run.                    │ │     │
│     │ │Results appear in Details as command.<field>; command.status  │ │     │
│     │ │shows Ready or Pending. Filters and field choices use the     │ │     │
│     │ │enrichment steps above.                                       │ │     │
│     │ │                                                              │ │     │
└─────│ └──────────────────────────────────────────────────────────────┘ │─────┘
      └──────────────────────────────────────────────────────────────────┘      

===== command-enrichment @ 54x16 focus=CommandEnrichment surface=Some(Rect { x: 5, y: 1, width: 44, height: 14 }) dialog_scroll=9 help_scroll=0 scroll_hitbox=Some(Rect { x: 6, y: 12, width: 42, height: 3 }) =====
lvu ┌ External command · runs only when confirmed┐    
┌ So│ Program: Executable path; no shell parsing │───┐
│● A│ /usr/bin/jq                                │   │
│  s│ Arguments: 1 line(s) · One argument per li │e r│
│ › │ -r '.trace_id'                             │e r│
│● W│ Working directory: Optional; defaults to t │e r│
│  s│ /home/user/work                            │e r│
│   │ Environment: 1 line(s) · Optional KEY=valu │e r│
│   │ Applied command step: None · enrichment st │e r│
│   │                                            │e r│
│   │ [ New line (Alt-N) ] [ Save ] [ Review ]   │e r│
│   │ [ Remove ]                                 │e r│
│   │ ┌ Status and review · ↑/↓ scroll ────────┐ │e r│
│   │ │Status: Unrun · Reviewed 16 sampled     │ │e r│
└───│ └────────────────────────────────────────┘ │───┘
    └────────────────────────────────────────────┘    
```

## Command palette and the small-terminal fallback

```text

===== bare @ 140x40 tiny=false =====
lvu log workspace DEMO FIXTURE — NOT ACQUISITION                                                                                            
┌ Sources / views ───┐┌ Log viewport ──────────────────────────────────────────────────────────────────────────────────────────────────────┐
│● API fixture       ││time          level  event                                                                                          │
│  synthetic/live    ││12:00:01      INFO   fixture request 01 completed                                                                   │
│ › All events       ││12:00:02      INFO   fixture request 02 completed                                                                   │
│● Worker fixture    ││12:00:03      INFO   Unicode 東 京  café é request completed                                                          │
│  synthetic/static  ││12:00:04      INFO   fixture request 04 completed                                                                   │
│   Errors only      ││12:00:05      WARN   fixture request 05 completed                                                                   │
│                    ││12:00:06      INFO   fixture request 06 completed                                                                   │
│                    ││12:00:07      INFO   fixture request 07 completed                                                                   │
│                    ││12:00:08      INFO   fixture request 08 completed                                                                   │
│                    ││12:00:09      INFO   fixture request 09 completed                                                                   │
│                    ││12:00:10      WARN   fixture request 10 completed                                                                   │
│                    ││12:00:11      INFO   fixture request 11 completed                                                                   │
│                    ││12:00:12      INFO   fixture request 12 completed                                                                   │
│                    ││12:00:13      INFO   fixture request 13 completed                                                                   │
│                    ││12:00:14      INFO   fixture request 14 completed                                                                   │
│                    ││12:00:15      WARN   fixture request 15 completed                                                                   │
│                    ││12:00:16      INFO   fixture request 16 completed                                                                   │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
│                    ││                                                                                                                    │
└────────────────────┘└────────────────────────────────────────────────────────────────────────────────────────────────────────────────────┘
                       FOLLOW | 1-16/16 | ? help                                                                                            

===== bare @ 100x30 tiny=false =====
lvu log workspace DEMO FIXTURE — NOT ACQUISITION                                                    
┌ Sources / views ───┐┌ Log viewport ──────────────────────────────────────────────────────────────┐
│● API fixture       ││time          level  event                                                  │
│  synthetic/live    ││12:00:01      INFO   fixture request 01 completed                           │
│ › All events       ││12:00:02      INFO   fixture request 02 completed                           │
│● Worker fixture    ││12:00:03      INFO   Unicode 東 京  café é request completed                  │
│  synthetic/static  ││12:00:04      INFO   fixture request 04 completed                           │
│   Errors only      ││12:00:05      WARN   fixture request 05 completed                           │
│                    ││12:00:06      INFO   fixture request 06 completed                           │
│                    ││12:00:07      INFO   fixture request 07 completed                           │
│                    ││12:00:08      INFO   fixture request 08 completed                           │
│                    ││12:00:09      INFO   fixture request 09 completed                           │
│                    ││12:00:10      WARN   fixture request 10 completed                           │
│                    ││12:00:11      INFO   fixture request 11 completed                           │
│                    ││12:00:12      INFO   fixture request 12 completed                           │
│                    ││12:00:13      INFO   fixture request 13 completed                           │
│                    ││12:00:14      INFO   fixture request 14 completed                           │
│                    ││12:00:15      WARN   fixture request 15 completed                           │
│                    ││12:00:16      INFO   fixture request 16 completed                           │
│                    ││                                                                            │
│                    ││                                                                            │
│                    ││                                                                            │
│                    ││                                                                            │
│                    ││                                                                            │
│                    ││                                                                            │
│                    ││                                                                            │
│                    ││                                                                            │
│                    ││                                                                            │
└────────────────────┘└────────────────────────────────────────────────────────────────────────────┘
                       FOLLOW | 1-16/16 | ? help                                                    

===== bare @ 80x24 tiny=false =====
lvu log workspace DEMO FIXTURE — NOT ACQUISITION                                
┌ Sources / views ───┐┌ Log viewport ──────────────────────────────────────────┐
│● API fixture       ││time          level  event                              │
│  synthetic/live    ││12:00:01      INFO   fixture request 01 completed       │
│ › All events       ││12:00:02      INFO   fixture request 02 completed       │
│● Worker fixture    ││12:00:03      INFO   Unicode 東 京  café é request complet│
│  synthetic/static  ││12:00:04      INFO   fixture request 04 completed       │
│   Errors only      ││12:00:05      WARN   fixture request 05 completed       │
│                    ││12:00:06      INFO   fixture request 06 completed       │
│                    ││12:00:07      INFO   fixture request 07 completed       │
│                    ││12:00:08      INFO   fixture request 08 completed       │
│                    ││12:00:09      INFO   fixture request 09 completed       │
│                    ││12:00:10      WARN   fixture request 10 completed       │
│                    ││12:00:11      INFO   fixture request 11 completed       │
│                    ││12:00:12      INFO   fixture request 12 completed       │
│                    ││12:00:13      INFO   fixture request 13 completed       │
│                    ││12:00:14      INFO   fixture request 14 completed       │
│                    ││12:00:15      WARN   fixture request 15 completed       │
│                    ││12:00:16      INFO   fixture request 16 completed       │
│                    ││                                                        │
│                    ││                                                        │
│                    ││                                                        │
└────────────────────┘└────────────────────────────────────────────────────────┘
                       FOLLOW | 1-16/16 | ? help                                

===== bare @ 54x16 tiny=false =====
lvu log workspace DEMO FIXTURE — NOT ACQUISITION      
┌ Sources / views ───┐┌ Log viewport ────────────────┐
│● API fixture       ││time          level  event    │
│  synthetic/live    ││12:00:06      INFO   fixture r│
│ › All events       ││12:00:07      INFO   fixture r│
│● Worker fixture    ││12:00:08      INFO   fixture r│
│  synthetic/static  ││12:00:09      INFO   fixture r│
│   Errors only      ││12:00:10      WARN   fixture r│
│                    ││12:00:11      INFO   fixture r│
│                    ││12:00:12      INFO   fixture r│
│                    ││12:00:13      INFO   fixture r│
│                    ││12:00:14      INFO   fixture r│
│                    ││12:00:15      WARN   fixture r│
│                    ││12:00:16      INFO   fixture r│
└────────────────────┘└──────────────────────────────┘
                       FOLLOW | 6-16/16 | ? help      

===== bare @ 30x8 tiny=false =====
lvu log workspace DEMO FIXTURE
┌ Log viewport ──────────────┐
│time          level  event  │
│12:00:14      INFO   fixture│
│12:00:15      WARN   fixture│
│12:00:16      INFO   fixture│
└────────────────────────────┘
 FOLLOW | 14-16/16 | ? help   

===== bare @ 18x5 tiny=true =====
lvu DEMO          
terminal too small
18x5  q quit      
                  
                  

===== palette @ 140x40 selection=Some(Rect { x: 25, y: 9, width: 90, height: 22 }) =====
                                                                                                                                            
                                                                                                                                            
                                                                                                                                            
                                                                                                                                            
                                                                                                                                            
                                                                                                                                            
                                                                                                                                            
                                                                                                                                            
                        ┌ Command palette · Ctrl-P ────────────────────────────────────────────────────────────────┐                        
                        │>                                                                                         │                        
                        │› Add source           n       Sources                                                    │                        
                        │  Advanced filter      p       Filter                                                     │                        
                        │  Ask agent            A       agent                                                      │                        
                        │  Bookmarks and notes  B       View                                                       │                        
                        │  Enrichment           e       Filter                                                     │                        
                        │  Fields               i       Fields                                                     │                        
                        │  Follow new records   f       View                                                       │                        
                        │  Grouping             m       Filter                                                     │                        
                        │  Help                 ?       Application                                                │                        
                        │  Investigations       I       agent                                                      │                        
                        │  Literal filter       /       Filter                                                     │                        
                        │  Manage views         v       Views                                                      │                        
                        │  Next view            ]       Views                                                      │                        
                        │  Previous view        [       Views                                                      │                        
                        │  Quit                 Ctrl-C  Application                                                │                        
                        │  Raw record context   o       View                                                       │                        
                        │  Recipes              r       Recipes                                                    │                        
                        │  Recognize timestamp          Agent                                                      │                        
                        │  Record details       d       View                                                       │                        
                        │Selected: Add source                                                                      │                        
                        │Open the admitted source dialog                                                           │                        
                        └──────────────────────────────────────────────────────────────────────────────────────────┘                        
                                                                                                                                            
                                                                                                                                            
                                                                                                                                            
                                                                                                                                            
                                                                                                                                            
                                                                                                                                            
                                                                                                                                            
                                                                                                                                            

===== palette @ 100x30 selection=Some(Rect { x: 5, y: 4, width: 90, height: 22 }) =====
                                                                                                    
                                                                                                    
                                                                                                    
    ┌ Command palette · Ctrl-P ────────────────────────────────────────────────────────────────┐    
    │>                                                                                         │    
    │› Add source           n       Sources                                                    │    
    │  Advanced filter      p       Filter                                                     │    
    │  Ask agent            A       agent                                                      │    
    │  Bookmarks and notes  B       View                                                       │    
    │  Enrichment           e       Filter                                                     │    
    │  Fields               i       Fields                                                     │    
    │  Follow new records   f       View                                                       │    
    │  Grouping             m       Filter                                                     │    
    │  Help                 ?       Application                                                │    
    │  Investigations       I       agent                                                      │    
    │  Literal filter       /       Filter                                                     │    
    │  Manage views         v       Views                                                      │    
    │  Next view            ]       Views                                                      │    
    │  Previous view        [       Views                                                      │    
    │  Quit                 Ctrl-C  Application                                                │    
    │  Raw record context   o       View                                                       │    
    │  Recipes              r       Recipes                                                    │    
    │  Recognize timestamp          Agent                                                      │    
    │  Record details       d       View                                                       │    
    │Selected: Add source                                                                      │    
    │Open the admitted source dialog                                                           │    
    └──────────────────────────────────────────────────────────────────────────────────────────┘    
                                                                                                    
                                                                                                    
                                                                                                    

===== palette @ 80x24 selection=Some(Rect { x: 1, y: 1, width: 78, height: 22 }) =====
┌ Command palette · Ctrl-P ────────────────────────────────────────────────────┐
│>                                                                             │
│› Add source           n       Sources                                        │
│  Advanced filter      p       Filter                                         │
│  Ask agent            A       agent                                          │
│  Bookmarks and notes  B       View                                           │
│  Enrichment           e       Filter                                         │
│  Fields               i       Fields                                         │
│  Follow new records   f       View                                           │
│  Grouping             m       Filter                                         │
│  Help                 ?       Application                                    │
│  Investigations       I       agent                                          │
│  Literal filter       /       Filter                                         │
│  Manage views         v       Views                                          │
│  Next view            ]       Views                                          │
│  Previous view        [       Views                                          │
│  Quit                 Ctrl-C  Application                                    │
│  Raw record context   o       View                                           │
│  Recipes              r       Recipes                                        │
│  Recognize timestamp          Agent                                          │
│  Record details       d       View                                           │
│Selected: Add source                                                          │
│Open the admitted source dialog                                               │
└──────────────────────────────────────────────────────────────────────────────┘

===== palette @ 54x16 selection=Some(Rect { x: 1, y: 1, width: 52, height: 14 }) =====
┌ Command palette · Ctrl-P ──────────────────────────┐
│>                                                   │
│› Add source           n  Sources                   │
│  Advanced filter      p  Filter                    │
│  Ask agent            A  agent                     │
│  Bookmarks and notes  B  View                      │
│  Enrichment           e  Filter                    │
│  Fields               i  Fields                    │
│  Follow new records   f  View                      │
│  Grouping             m  Filter                    │
│  Help                 ?  Application               │
│  Investigations       I  agent                     │
│  Literal filter       /  Filter                    │
│Selected: Add source                                │
│Open the admitted source dialog                     │
└────────────────────────────────────────────────────┘
```

## Overflowing single-line inputs at 54x16

Draft in every case: `ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789`.

```text

===== search (draft = ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789) =====
lvu log workspace DEMO FIXTURE — NOT ACQUISITION      
┌ Sources / views ───┐┌ Log viewport ────────────────┐
│● AP┌ Search ─────────────────────────────────┐t    │
│  sy│ YZabcdefghijklmnopqrstuvwxyz01234567899 │ure r│
│ › A│ Applied  No filter applied.             │ure r│
│● Wo│                                         │ure r│
│  sy│                                         │ure r│
│   E│                                         │ure r│
│    │                                         │ure r│
│    │ Examples: text · "field name": text ·   │ure r│
│    │ /regex/ims · \/literal                  │ure r│
│    │                                         │ure r│
│    └─────────────────────────────────────────┘ure r│
│                    ││12:00:16      INFO   fixture r│
└────────────────────┘└──────────────────────────────┘
                       FOLLOW | 6-16/16 | ? help      

===== views-prompt (draft = ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789) =====
lvu log workspace DEMO FIXTURE — NOT ACQUISITION      
┌ Sources / views ───┐┌ Log viewport ────────────────┐
│● API fixture       ││time          level  event    │
│  syn┌ Source view ──────────────────────────┐ture r│
│ › Al│ Mode: CLONE SETTINGS                  │ture r│
│● Wor│                                       │ture r│
│  syn│ Name: ghijklmnopqrstuvwxyz0123456789M │ture r│
│   Er│ eventsABCDEFGHIJKLMN                  │ture r│
│     │                                       │ture r│
│     │[ New blank ]w[ Clone ]g[ Rename ] and │ture r│
│     │[ Sources ]r[ Apply ]e source capture. │ture r│
│     │                                       │ture r│
│     └───────────────────────────────────────┘ture r│
│                    ││12:00:16      INFO   fixture r│
└────────────────────┘└──────────────────────────────┘
                       FOLLOW | 6-16/16 | ? help      

===== source-manual (draft = ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789) =====
┌ Add source ────────────────────────────────────────┐
│ FILE PATH                                          │
│                                                    │
│ NOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz01234567899 │
│                                                    │
│ Completing path…                                   │
│                                                    │
│                                                    │
│                                                    │
│ ┌ State ─────────────────────────────────────────┐ │
│ │Ready: provide a file path or command. Capture  │ │
│ └────────────────────────────────────────────────┘ │
│                                                    │
│ [ Manual ] [ Discover ] [ 🧠  ] [ File ]            │
│ [ Command ]                                        │
└────────────────────────────────────────────────────┘
```

## Input-surface cell counts at 100x30

Per modal row: cells painted with `theme.input_bg` versus `theme.dialog_bg`.

```text

===== help @ 100x30 modal=Rect { x: 4, y: 2, width: 92, height: 26 } =====
  row  2: input_bg=  0 dialog_bg= 92 other=[] |  EVERYWHERE
  row  3: input_bg=  0 dialog_bg= 92 other=[] |    Ctrl-P               Open the command palette
  row  4: input_bg=  0 dialog_bg= 92 other=[] |    ?                    Open or close this help
  row  5: input_bg=  0 dialog_bg= 92 other=[] |    Ctrl-L               Redraw the terminal
  row  6: input_bg=  0 dialog_bg= 92 other=[] |    ,                    Open settings
  row  7: input_bg=  0 dialog_bg= 92 other=[] |    q / Ctrl-C           Quit
  row  8: input_bg=  0 dialog_bg= 92 other=[] | 
  row  9: input_bg=  0 dialog_bg= 92 other=[] |  LOGS & VIEWS
  row 10: input_bg=  0 dialog_bg= 92 other=[] |    g / G                Jump to first / last record
  row 11: input_bg=  0 dialog_bg= 92 other=[] |    ←/→ · 0              Pan the selected event / reset pan
  row 12: input_bg=  0 dialog_bg= 92 other=[] |    [ / ]                Previous or next view
  row 13: input_bg=  0 dialog_bg= 92 other=[] |    f                    Toggle follow / history
  row 14: input_bg=  0 dialog_bg= 92 other=[] |    d                    Toggle selected-record details
  row 15: input_bg=  0 dialog_bg= 92 other=[] |    o                    Open raw context
  row 16: input_bg=  0 dialog_bg= 92 other=[] |    b                    Toggle a bookmark
  row 17: input_bg=  0 dialog_bg= 92 other=[] |    B                    Open bookmarks and notes
  row 18: input_bg=  0 dialog_bg= 92 other=[] |    Alt-S                Stop the selected source
  row 19: input_bg=  0 dialog_bg= 92 other=[] |    Alt-R                Restart the selected source
  row 20: input_bg=  0 dialog_bg= 92 other=[] | 
  row 21: input_bg=  0 dialog_bg= 92 other=[] |  FILTER & SHAPE
  row 22: input_bg=  0 dialog_bg= 92 other=[] |    /                    Literal or field-aware search
  row 23: input_bg=  0 dialog_bg= 92 other=[] |    p                    Open the advanced filter
  row 24: input_bg=  0 dialog_bg= 92 other=[] |    e                    Open ordered enrichments
  row 25: input_bg=  0 dialog_bg= 92 other=[] |    Alt-C in Enrichment  Add, edit, remove, or explicitly run the terminal command step
  row 26: input_bg=  0 dialog_bg= 92 other=[] |    m                    Open display-only grouping
  row 27: input_bg=  0 dialog_bg= 92 other=[] | ↑/↓ or j/k scroll · ? close

===== context-raw @ 100x30 modal=Rect { x: 1, y: 3, width: 98, height: 24 } =====
  row  3: input_bg=  0 dialog_bg= 98 other=[] |  Anchor: api:16 · physical source records
  row  4: input_bg=  0 dialog_bg= 98 other=[] |  11–16 / 16 · raw, unfiltered, ungrouped
  row  5: input_bg=  0 dialog_bg= 98 other=[] |        11 fixture request 11 completed
  row  6: input_bg=  0 dialog_bg= 98 other=[] |        12 fixture request 12 completed
  row  7: input_bg=  0 dialog_bg= 98 other=[] |        13 fixture request 13 completed
  row  8: input_bg=  0 dialog_bg= 98 other=[] |        14 fixture request 14 completed
  row  9: input_bg=  0 dialog_bg= 98 other=[] |        15 fixture request 15 completed
  row 10: input_bg=  0 dialog_bg= 61 other=[Yellow] |  >     16 fixture request 16 completed
  row 11: input_bg=  0 dialog_bg= 98 other=[] | 
  row 12: input_bg=  0 dialog_bg= 98 other=[] | 
  row 13: input_bg=  0 dialog_bg= 98 other=[] | 
  row 14: input_bg=  0 dialog_bg= 98 other=[] | 
  row 15: input_bg=  0 dialog_bg= 98 other=[] | 
  row 16: input_bg=  0 dialog_bg= 98 other=[] | 
  row 17: input_bg=  0 dialog_bg= 98 other=[] | 
  row 18: input_bg=  0 dialog_bg= 98 other=[] | 
  row 19: input_bg=  0 dialog_bg= 98 other=[] | 
  row 20: input_bg=  0 dialog_bg= 98 other=[] | 
  row 21: input_bg=  0 dialog_bg= 98 other=[] | 
  row 22: input_bg=  0 dialog_bg= 98 other=[] | 
  row 23: input_bg=  0 dialog_bg= 98 other=[] | 
  row 24: input_bg=  0 dialog_bg= 98 other=[] | 
  row 25: input_bg=  0 dialog_bg= 98 other=[] | 
  row 26: input_bg=  0 dialog_bg= 98 other=[] | ↑/↓ scroll · g anchor

===== fields @ 100x30 modal=Rect { x: 16, y: 8, width: 68, height: 14 } =====
  row  8: input_bg=  0 dialog_bg= 49 other=[Yellow] |  > [ ] service = api
  row  9: input_bg=  0 dialog_bg= 68 other=[] |    [ ] level = INFO
  row 10: input_bg=  0 dialog_bg= 68 other=[] | 
  row 11: input_bg=  0 dialog_bg= 68 other=[] | 
  row 12: input_bg=  0 dialog_bg= 68 other=[] | 
  row 13: input_bg=  0 dialog_bg= 68 other=[] | 
  row 14: input_bg=  0 dialog_bg= 68 other=[] | 
  row 15: input_bg=  0 dialog_bg= 68 other=[] | 
  row 16: input_bg=  0 dialog_bg= 68 other=[] | 
  row 17: input_bg=  0 dialog_bg= 68 other=[] | 
  row 18: input_bg=  0 dialog_bg= 68 other=[] | 
  row 19: input_bg=  0 dialog_bg= 68 other=[] | 
  row 20: input_bg=  0 dialog_bg= 68 other=[] | 
  row 21: input_bg=  0 dialog_bg= 68 other=[] | ↑/↓ select · Space pin · c Color rows by this field

===== search @ 100x30 modal=Rect { x: 11, y: 10, width: 78, height: 9 } =====
  row 10: input_bg= 75 dialog_bg=  2 other=[Yellow] |  level: WARN
  row 11: input_bg=  0 dialog_bg= 78 other=[] |  Applied  No filter applied.
  row 12: input_bg=  0 dialog_bg= 78 other=[] | 
  row 13: input_bg=  0 dialog_bg= 78 other=[] | 
  row 14: input_bg=  0 dialog_bg= 78 other=[] | 
  row 15: input_bg=  0 dialog_bg= 78 other=[] | 
  row 16: input_bg=  0 dialog_bg= 78 other=[] |  Examples: text · "field name": text · /regex/ims · \/literal
  row 17: input_bg=  0 dialog_bg= 78 other=[] | 
  row 18: input_bg=  0 dialog_bg= 78 other=[] | 

===== advanced @ 100x30 modal=Rect { x: 11, y: 10, width: 78, height: 9 } =====
  row 10: input_bg=  0 dialog_bg= 78 other=[] |  FILTER EXPRESSION
  row 11: input_bg= 75 dialog_bg=  2 other=[Yellow] |  col("level").eq(lit("WARN"))
  row 12: input_bg=  0 dialog_bg= 78 other=[] |  Applied  No filter applied.
  row 13: input_bg=  0 dialog_bg= 78 other=[] | 
  row 14: input_bg=  0 dialog_bg= 78 other=[] | 
  row 15: input_bg=  0 dialog_bg= 78 other=[] | 
  row 16: input_bg=  0 dialog_bg= 78 other=[] |  Use a Polars expression. Fields and static sampled literals are available as
  row 17: input_bg=  0 dialog_bg= 78 other=[] |  completions.
  row 18: input_bg=  0 dialog_bg= 78 other=[] | 

===== grouping @ 100x30 modal=Rect { x: 11, y: 9, width: 78, height: 11 } =====
  row  9: input_bg=  0 dialog_bg= 78 other=[] |  Continuation regex over raw bytes
  row 10: input_bg= 75 dialog_bg=  2 other=[Yellow] |  ^(\s+|Caused by:)
  row 11: input_bg=  0 dialog_bg= 78 other=[] |  Applied: Grouping disabled.
  row 12: input_bg=  0 dialog_bg= 78 other=[] | 
  row 13: input_bg=  0 dialog_bg= 78 other=[] | 
  row 14: input_bg=  0 dialog_bg= 78 other=[] | 
  row 15: input_bg=  0 dialog_bg= 78 other=[] |  Preview (display only):
  row 16: input_bg=  0 dialog_bg= 78 other=[] |  RuntimeException: boom
  row 17: input_bg=  0 dialog_bg= 78 other=[] |    at worker.rs:42  → 2 physical lines
  row 18: input_bg=  0 dialog_bg= 78 other=[] |  Empty draft disables grouping
  row 19: input_bg=  0 dialog_bg= 78 other=[] | 

===== time @ 100x30 modal=Rect { x: 7, y: 5, width: 86, height: 20 } =====
  row  5: input_bg=  0 dialog_bg= 86 other=[] | 
  row  6: input_bg=  0 dialog_bg= 61 other=[Yellow] |  [ Time basis: Capture ▾ ]
  row  7: input_bg=  0 dialog_bg= 86 other=[] |  [ Window: All time ▾ ]
  row  8: input_bg=  0 dialog_bg= 86 other=[] | 
  row  9: input_bg= 62 dialog_bg= 24 other=[] |  Start 1969-12-31 23:59:46.000000000                                   UTC      [ ▾ ]
  row 10: input_bg= 62 dialog_bg= 24 other=[] |  End   1970-01-01 00:00:46.000000000                                   UTC      [ ▾ ]
  row 11: input_bg=  0 dialog_bg= 86 other=[] | 
  row 12: input_bg=  0 dialog_bg= 86 other=[] |  [ Apply ] [ Clear ] [ 🧠  Recognize timestamp ]
  row 13: input_bg=  0 dialog_bg= 86 other=[] | 
  row 14: input_bg=  0 dialog_bg= 86 other=[] |  ┌ Applied ─────────────────────────────────────────────────────────────────────────┐
  row 15: input_bg=  0 dialog_bg= 86 other=[] |  │Applied: all times                                                                │
  row 16: input_bg=  0 dialog_bg= 86 other=[] |  └──────────────────────────────────────────────────────────────────────────────────┘
  row 17: input_bg=  0 dialog_bg= 86 other=[] | 
  row 18: input_bg=  0 dialog_bg= 86 other=[] |  Bounds are half-open. UTC and numeric offsets are normalized to UTC; named zones are
  row 19: input_bg=  0 dialog_bg= 86 other=[] |   not supported.
  row 20: input_bg=  0 dialog_bg= 86 other=[] | 
  row 21: input_bg=  0 dialog_bg= 86 other=[] | 
  row 22: input_bg=  0 dialog_bg= 86 other=[] | 
  row 23: input_bg=  0 dialog_bg= 86 other=[] | 
  row 24: input_bg=  0 dialog_bg= 86 other=[] | 

===== settings @ 100x30 modal=Rect { x: 1, y: 1, width: 98, height: 28 } =====
  row  1: input_bg=  0 dialog_bg= 98 other=[] |  🧠  configuration
  row  2: input_bg= 60 dialog_bg= 37 other=[Yellow] |  Provider/model: ex/gpt-5.6-sol  Mode: full-access               Thinking: medium
  row  3: input_bg=  0 dialog_bg= 98 other=[] | 
  row  4: input_bg=  0 dialog_bg= 98 other=[] |  Appearance
  row  5: input_bg=  0 dialog_bg= 98 other=[] |  [ Theme: terminal ▾ ] [ Delight: On ] [ Reduced motion: Off ] [ ASCII: Off ]
  row  6: input_bg=  0 dialog_bg= 98 other=[] | 
  row  7: input_bg=  0 dialog_bg= 98 other=[] | 
  row  8: input_bg=  0 dialog_bg= 98 other=[] |  Cache limits (MiB)
  row  9: input_bg= 76 dialog_bg= 22 other=[] |  Rows: 4                                         Membership: 256
  row 10: input_bg= 67 dialog_bg= 31 other=[] |  Derived total: 5120                             Per source: 256
  row 11: input_bg=  0 dialog_bg= 98 other=[] |  [ Save ]
  row 12: input_bg=  0 dialog_bg= 98 other=[] |  ┌ State ───────────────────────────────────────────────────────────────────────────────────────┐
  row 13: input_bg=  0 dialog_bg= 98 other=[] |  │Saved: Saved; restart required for cache-limit changes                                        │
  row 14: input_bg=  0 dialog_bg= 98 other=[] |  │Saved settings loaded; cache-limit changes apply after restart                                │
  row 15: input_bg=  0 dialog_bg= 98 other=[] |  └──────────────────────────────────────────────────────────────────────────────────────────────┘
  row 16: input_bg=  0 dialog_bg= 98 other=[] |  ┌ Effective values and paths ──────────────────────────────────────────────────────────────────┐
  row 17: input_bg=  0 dialog_bg= 98 other=[] |  │Effective 🧠 : codex/env [environment LVU_AI_PROVIDER] · full-access [settings.toml] · medium  │
  row 18: input_bg=  0 dialog_bg= 98 other=[] |  │[settings.toml]                                                                               │
  row 19: input_bg=  0 dialog_bg= 98 other=[] |  │Effective appearance: theme terminal · delight false [environment LVU_NO_DELIGHT] · motion    │
  row 20: input_bg=  0 dialog_bg= 98 other=[] |  │true [environment LVU_REDUCED_MOTION] · ASCII false [settings.toml]                           │
  row 21: input_bg=  0 dialog_bg= 98 other=[] |  │Startup-applied MiB: rows 4 · membership 256 · total derived 5120 · index/source 256          │
  row 22: input_bg=  0 dialog_bg= 98 other=[] |  │Settings: /home/user/.config/lvu/settings.toml                                                │
  row 23: input_bg=  0 dialog_bg= 98 other=[] |  │Data: /home/user/.local/share/lvu                                                             │
  row 24: input_bg=  0 dialog_bg= 98 other=[] |  │Cache: /home/user/.cache/lvu                                                                  │
  row 25: input_bg=  0 dialog_bg= 98 other=[] |  │Capture: /home/user/.local/share/lvu/captures                                                 │
  row 26: input_bg=  0 dialog_bg= 98 other=[] |  │Cache-limit changes take effect after restart; appearance previews immediately.               │
  row 27: input_bg=  0 dialog_bg= 98 other=[] |  └──────────────────────────────────────────────────────────────────────────────────────────────┘
  row 28: input_bg=  0 dialog_bg= 98 other=[] | 

===== storage @ 100x30 modal=Rect { x: 7, y: 6, width: 86, height: 18 } =====
  row  6: input_bg=  0 dialog_bg= 86 other=[] |  row cache 4.0 MiB / 4.0 MiB   query membership 2.0 MiB / 256.0 MiB
  row  7: input_bg=  0 dialog_bg= 86 other=[] |  derived disk cap/source 256.0 MiB · global 5.0 GiB
  row  8: input_bg=  0 dialog_bg= 86 other=[] |  managed budgets; not a process RSS limit
  row  9: input_bg=  0 dialog_bg= 86 other=[] | 
  row 10: input_bg=  0 dialog_bg=  2 other=[Yellow] |  > derived     1.0 MiB api-fixture/00.rows.idx — unused, recomputable from capture
  row 11: input_bg=  0 dialog_bg= 86 other=[] |    capture     2.0 MiB api-fixture/01.rows.idx — unused, recomputable from capture re
  row 12: input_bg=  0 dialog_bg= 86 other=[] |    derived     3.0 MiB api-fixture/02.rows.idx — unused, recomputable from capture re
  row 13: input_bg=  0 dialog_bg= 86 other=[] |    capture     4.0 MiB api-fixture/03.rows.idx — unused, recomputable from capture re
  row 14: input_bg=  0 dialog_bg= 86 other=[] |    derived     5.0 MiB api-fixture/04.rows.idx — unused, recomputable from capture re
  row 15: input_bg=  0 dialog_bg= 86 other=[] |    capture     6.0 MiB api-fixture/05.rows.idx — unused, recomputable from capture re
  row 16: input_bg=  0 dialog_bg= 86 other=[] |    derived     7.0 MiB api-fixture/06.rows.idx — unused, recomputable from capture re
  row 17: input_bg=  0 dialog_bg= 86 other=[] |    capture     8.0 MiB api-fixture/07.rows.idx — unused, recomputable from capture re
  row 18: input_bg=  0 dialog_bg= 86 other=[] |    derived     9.0 MiB api-fixture/08.rows.idx — unused, recomputable from capture re
  row 19: input_bg=  0 dialog_bg= 86 other=[] | 
  row 20: input_bg=  0 dialog_bg= 86 other=[] |  ┌ Status ──────────────────────────────────────────────────────────────────────────┐
  row 21: input_bg=  0 dialog_bg= 86 other=[] |  │Status: complete                                                                  │
  row 22: input_bg=  0 dialog_bg= 86 other=[] |  └──────────────────────────────────────────────────────────────────────────────────┘
  row 23: input_bg=  0 dialog_bg= 86 other=[] | ↑/↓ active pane · r refresh · c preview/confirm cleanup

===== recipes @ 100x30 modal=Rect { x: 9, y: 6, width: 82, height: 18 } =====
  row  6: input_bg=  0 dialog_bg= 82 other=[] |  > weekly error triage 0 @ rev00000
  row  7: input_bg=  0 dialog_bg= 82 other=[] |    weekly error triage 1 @ rev10000
  row  8: input_bg=  0 dialog_bg= 82 other=[] |    weekly error triage 2 @ rev20000
  row  9: input_bg=  0 dialog_bg= 82 other=[] |    weekly error triage 3 @ rev30000
  row 10: input_bg=  0 dialog_bg= 82 other=[] |    weekly error triage 4 @ rev40000
  row 11: input_bg=  0 dialog_bg= 82 other=[] |    weekly error triage 5 @ rev50000
  row 12: input_bg=  0 dialog_bg= 82 other=[] |  No applicable similar-source suggestions; all recipes remain browsable.
  row 13: input_bg=  0 dialog_bg= 82 other=[] |  Preview search="" advanced=false enrichment=false pins= color=none
  row 14: input_bg=  0 dialog_bg= 82 other=[] |  capture-time=all
  row 15: input_bg=  0 dialog_bg= 82 other=[] |  Applied: 6 saved recipes · Enter applies the selected revision
  row 16: input_bg=  0 dialog_bg= 82 other=[] | 
  row 17: input_bg=  0 dialog_bg= 82 other=[] | 
  row 18: input_bg=  0 dialog_bg= 82 other=[] | 
  row 19: input_bg=  0 dialog_bg= 82 other=[] | 
  row 20: input_bg=  0 dialog_bg= 82 other=[] | 
  row 21: input_bg=  0 dialog_bg= 82 other=[] | [ Browse ] [ Save ] [ Import ] [ Export ] [ History ] [ Update ] [ Refresh ]
  row 22: input_bg=  0 dialog_bg= 82 other=[] | [ Apply revision ]
  row 23: input_bg=  0 dialog_bg= 82 other=[] | 

===== bookmarks @ 100x30 modal=Rect { x: 1, y: 5, width: 98, height: 20 } =====
  row  5: input_bg=  0 dialog_bg= 98 other=[] |  1 / 128 bookmarks ·
  row  6: input_bg=  0 dialog_bg= 98 other=[] |  Record api:16
  row  7: input_bg=  0 dialog_bg= 98 other=[] | 
  row  8: input_bg=  0 dialog_bg= 98 other=[] | 
  row  9: input_bg=  0 dialog_bg= 98 other=[] | 
  row 10: input_bg=  0 dialog_bg= 98 other=[] | 
  row 11: input_bg=  0 dialog_bg= 98 other=[] | 
  row 12: input_bg=  0 dialog_bg= 98 other=[] | 
  row 13: input_bg=  0 dialog_bg= 98 other=[] | 
  row 14: input_bg=  0 dialog_bg= 98 other=[] | 
  row 15: input_bg=  0 dialog_bg= 98 other=[] | 
  row 16: input_bg=  0 dialog_bg= 98 other=[] | 
  row 17: input_bg=  0 dialog_bg= 98 other=[] | 
  row 18: input_bg=  0 dialog_bg= 98 other=[] | 
  row 19: input_bg=  0 dialog_bg= 98 other=[] | 
  row 20: input_bg=  0 dialog_bg= 98 other=[] | 
  row 21: input_bg=  0 dialog_bg= 98 other=[] |  Note (1024 bytes)
  row 22: input_bg= 95 dialog_bg=  2 other=[Yellow] |  checked with the on-call rotation; correlates with the 12:00 deploy
  row 23: input_bg=  0 dialog_bg= 98 other=[] |  [ Save note ]
  row 24: input_bg=  0 dialog_bg= 98 other=[] | ↑/↓ select

===== views-prompt @ 100x30 modal=Rect { x: 13, y: 11, width: 74, height: 8 } =====
  row 11: input_bg=  0 dialog_bg= 74 other=[] |  Mode: CLONE SETTINGS
  row 12: input_bg=  0 dialog_bg= 74 other=[] | 
  row 13: input_bg= 65 dialog_bg=  8 other=[Yellow] |  Name: Copy of All events
  row 14: input_bg=  0 dialog_bg= 74 other=[] | 
  row 15: input_bg=  0 dialog_bg= 74 other=[] |  Name the view. Creating, cloning, and renaming preserve the source
  row 16: input_bg=  0 dialog_bg= 74 other=[] | [ New blank ] [ Clone ] [ Rename ] [ Sources ] [ Apply ]
  row 17: input_bg=  0 dialog_bg= 74 other=[] | 
  row 18: input_bg=  0 dialog_bg= 74 other=[] | 

===== views-sources @ 100x30 modal=Rect { x: 4, y: 5, width: 92, height: 20 } =====
  row  5: input_bg=  0 dialog_bg= 92 other=[] |  Order: source position, then record sequence (not clock order).
  row  6: input_bg=  0 dialog_bg= 92 other=[] |  The owning source remains included; captures are shared.
  row  7: input_bg=  0 dialog_bg= 74 other=[Yellow] |  [x]  1 API fixture
  row  8: input_bg=  0 dialog_bg= 92 other=[] |  [ ]    Worker fixture
  row  9: input_bg=  0 dialog_bg= 92 other=[] | 
  row 10: input_bg=  0 dialog_bg= 92 other=[] | 
  row 11: input_bg=  0 dialog_bg= 92 other=[] | 
  row 12: input_bg=  0 dialog_bg= 92 other=[] | 
  row 13: input_bg=  0 dialog_bg= 92 other=[] | 
  row 14: input_bg=  0 dialog_bg= 92 other=[] | 
  row 15: input_bg=  0 dialog_bg= 92 other=[] | 
  row 16: input_bg=  0 dialog_bg= 92 other=[] | 
  row 17: input_bg=  0 dialog_bg= 92 other=[] | 
  row 18: input_bg=  0 dialog_bg= 92 other=[] | 
  row 19: input_bg=  0 dialog_bg= 92 other=[] | 
  row 20: input_bg=  0 dialog_bg= 92 other=[] | 
  row 21: input_bg=  0 dialog_bg= 92 other=[] | 
  row 22: input_bg=  0 dialog_bg= 92 other=[] | [ New blank ] [ Clone ] [ Rename ] [ Sources ] [ Apply membership ]
  row 23: input_bg=  0 dialog_bg= 92 other=[] | 
  row 24: input_bg=  0 dialog_bg= 92 other=[] | 

===== source-manual @ 100x30 modal=Rect { x: 1, y: 4, width: 98, height: 22 } =====
  row  4: input_bg=  0 dialog_bg= 98 other=[] |  FILE PATH
  row  5: input_bg=  0 dialog_bg= 98 other=[] | 
  row  6: input_bg= 95 dialog_bg=  2 other=[Yellow] | 
  row  7: input_bg=  0 dialog_bg= 98 other=[] | 
  row  8: input_bg=  0 dialog_bg= 98 other=[] | 
  row  9: input_bg=  0 dialog_bg= 98 other=[] | 
  row 10: input_bg=  0 dialog_bg= 98 other=[] | 
  row 11: input_bg=  0 dialog_bg= 98 other=[] | 
  row 12: input_bg=  0 dialog_bg= 98 other=[] | 
  row 13: input_bg=  0 dialog_bg= 98 other=[] | 
  row 14: input_bg=  0 dialog_bg= 98 other=[] | 
  row 15: input_bg=  0 dialog_bg= 98 other=[] | 
  row 16: input_bg=  0 dialog_bg= 98 other=[] | 
  row 17: input_bg=  0 dialog_bg= 98 other=[] | 
  row 18: input_bg=  0 dialog_bg= 98 other=[] | 
  row 19: input_bg=  0 dialog_bg= 98 other=[] | 
  row 20: input_bg=  0 dialog_bg= 98 other=[] |  ┌ State ───────────────────────────────────────────────────────────────────────────────────────┐
  row 21: input_bg=  0 dialog_bg= 98 other=[] |  │Ready: provide a file path or command. Capture starts only after submission.                  │
  row 22: input_bg=  0 dialog_bg= 98 other=[] |  └──────────────────────────────────────────────────────────────────────────────────────────────┘
  row 23: input_bg=  0 dialog_bg= 98 other=[] | 
  row 24: input_bg=  0 dialog_bg= 98 other=[] |  [ Manual ] [ Discover ] [ 🧠  ] [ File ] [ Command ]
  row 25: input_bg=  0 dialog_bg= 98 other=[] | 

===== source-discovery @ 100x30 modal=Rect { x: 1, y: 4, width: 98, height: 22 } =====
  row  4: input_bg= 88 dialog_bg=  9 other=[Yellow] |  Search
  row  5: input_bg=  0 dialog_bg= 98 other=[] |  0/0 matches · selection never starts capture
  row  6: input_bg=  0 dialog_bg= 98 other=[] |    No matching candidates.
  row  7: input_bg=  0 dialog_bg= 98 other=[] | 
  row  8: input_bg=  0 dialog_bg= 98 other=[] | 
  row  9: input_bg=  0 dialog_bg= 98 other=[] | 
  row 10: input_bg=  0 dialog_bg= 98 other=[] | 
  row 11: input_bg=  0 dialog_bg= 98 other=[] | 
  row 12: input_bg=  0 dialog_bg= 98 other=[] | 
  row 13: input_bg=  0 dialog_bg= 98 other=[] | 
  row 14: input_bg=  0 dialog_bg= 98 other=[] | 
  row 15: input_bg=  0 dialog_bg= 98 other=[] | 
  row 16: input_bg=  0 dialog_bg= 98 other=[] | 
  row 17: input_bg=  0 dialog_bg= 98 other=[] | 
  row 18: input_bg=  0 dialog_bg= 98 other=[] |  ┌ Diagnostics ─────────────────────────────────────────────────────────────────────────────────┐
  row 19: input_bg=  0 dialog_bg= 98 other=[] |  │No candidate selected.                                                                        │
  row 20: input_bg=  0 dialog_bg= 98 other=[] |  │UPDATING: scanning bounded local providers…                                                   │
  row 21: input_bg=  0 dialog_bg= 98 other=[] |  │                                                                                              │
  row 22: input_bg=  0 dialog_bg= 98 other=[] |  └──────────────────────────────────────────────────────────────────────────────────────────────┘
  row 23: input_bg=  0 dialog_bg= 98 other=[] | 
  row 24: input_bg=  0 dialog_bg= 98 other=[] |  [ Manual ] [ Discover ] [ 🧠  ] [ Refresh ]
  row 25: input_bg=  0 dialog_bg= 98 other=[] | 

===== source-ai-proposal @ 100x30 modal=Rect { x: 1, y: 4, width: 98, height: 22 } =====
  row  4: input_bg=  0 dialog_bg= 98 other=[] |  Request
  row  5: input_bg= 96 dialog_bg=  2 other=[] |  tail the nginx access log
  row  6: input_bg=  0 dialog_bg= 98 other=[] |  ┌ State ───────────────────────────────────────────────────────────────────────────────────────┐
  row  7: input_bg=  0 dialog_bg= 98 other=[] |  │Proposal: Review only — explicit confirmation starts this source                              │
  row  8: input_bg=  0 dialog_bg= 98 other=[] |  └──────────────────────────────────────────────────────────────────────────────────────────────┘
  row  9: input_bg=  0 dialog_bg= 98 other=[] |  ┌ Preview ─────────────────────────────────────────────────────────────────────────────────────┐
  row 10: input_bg=  0 dialog_bg= 98 other=[] |  │Name: nginx access log                                                                        │
  row 11: input_bg=  0 dialog_bg= 98 other=[] |  │Kind: command                                                                                 │
  row 12: input_bg=  0 dialog_bg= 98 other=[] |  │Launch: tail -F /var/log/nginx/access.log                                                     │
  row 13: input_bg=  0 dialog_bg= 98 other=[] |  │Effective path/cwd: /var/log/nginx                                                            │
  row 14: input_bg=  0 dialog_bg= 98 other=[] |  │Restart: restart on exit with backoff                                                         │
  row 15: input_bg=  0 dialog_bg= 98 other=[] |  │Env: TZ=UTC                                                                                   │
  row 16: input_bg=  0 dialog_bg= 98 other=[] |  │Env: LANG=C.UTF-8                                                                             │
  row 17: input_bg=  0 dialog_bg= 98 other=[] |  │Env: NGINX_LOG_FORMAT=combined                                                                │
  row 18: input_bg=  0 dialog_bg= 98 other=[] |  │Why: Follows the running access log without reading historical rotations. Capture keeps the   │
  row 19: input_bg=  0 dialog_bg= 98 other=[] |  │original bytes; the view adds no filter.                                                      │
  row 20: input_bg=  0 dialog_bg= 98 other=[] |  │                                                                                              │
  row 21: input_bg=  0 dialog_bg= 98 other=[] |  └──────────────────────────────────────────────────────────────────────────────────────────────┘
  row 22: input_bg=  0 dialog_bg= 98 other=[] |  Describe a source; review is required before capture starts.
  row 23: input_bg=  0 dialog_bg= 98 other=[] | 
  row 24: input_bg=  0 dialog_bg= 80 other=[Yellow] |  [ Start reviewed ] [ Manual ] [ Discover ] [ 🧠  ]
  row 25: input_bg=  0 dialog_bg= 98 other=[] | 

===== ask-ai-proposal @ 100x30 modal=Rect { x: 4, y: 4, width: 92, height: 22 } =====
  row  4: input_bg=  0 dialog_bg= 92 other=[] |  [ Kind: Filter ▾ ] [ Apply ]
  row  5: input_bg=  0 dialog_bg= 92 other=[] |  ┌ Request ───────────────────────────────────────────────────────────────────────────────┐
  row  6: input_bg=  0 dialog_bg= 92 other=[] |  │only warnings from the api service                                                      │
  row  7: input_bg=  0 dialog_bg= 92 other=[] |  │                                                                                        │
  row  8: input_bg=  0 dialog_bg= 92 other=[] |  │                                                                                        │
  row  9: input_bg=  0 dialog_bg= 92 other=[] |  │                                                                                        │
  row 10: input_bg=  0 dialog_bg= 92 other=[] |  └────────────────────────────────────────────────────────────────────────────────────────┘
  row 11: input_bg=  0 dialog_bg= 92 other=[] |  ┌ State ─────────────────────────────────────────────────────────────────────────────────┐
  row 12: input_bg=  0 dialog_bg= 92 other=[] |  │Proposal: Proposal ready — review before applying                                       │
  row 13: input_bg=  0 dialog_bg= 92 other=[] |  │                                                                                        │
  row 14: input_bg=  0 dialog_bg= 92 other=[] |  └────────────────────────────────────────────────────────────────────────────────────────┘
  row 15: input_bg=  0 dialog_bg= 92 other=[] |  ┌ Proposal and activity ─────────────────────────────────────────────────────────────────┐
  row 16: input_bg=  0 dialog_bg= 92 other=[] |  │Agent: codex/gpt-5.6-sol · mode full-access · thinking medium                           │
  row 17: input_bg=  0 dialog_bg= 92 other=[] |  │Submitted request: only warnings from the api service                                   │
  row 18: input_bg=  0 dialog_bg= 92 other=[] |  │Proposal: col("level").eq(lit("WARN")).and(col("service").eq(lit("api")))               │
  row 19: input_bg=  0 dialog_bg= 92 other=[] |  │Explanation: Keeps WARN rows emitted by the api service. Other services and other levels│
  row 20: input_bg=  0 dialog_bg= 92 other=[] |  │stay excluded; the accepted filter is unchanged until you apply this.                   │
  row 21: input_bg=  0 dialog_bg= 92 other=[] |  │                                                                                        │
  row 22: input_bg=  0 dialog_bg= 92 other=[] |  │                                                                                        │
  row 23: input_bg=  0 dialog_bg= 92 other=[] |  │                                                                                        │
  row 24: input_bg=  0 dialog_bg= 92 other=[] |  └────────────────────────────────────────────────────────────────────────────────────────┘
  row 25: input_bg=  0 dialog_bg= 92 other=[] | 

===== investigation @ 100x30 modal=Rect { x: 4, y: 4, width: 92, height: 22 } =====
  row  4: input_bg=  0 dialog_bg= 92 other=[] |  [ Send ]
  row  5: input_bg=  0 dialog_bg= 92 other=[] | 
  row  6: input_bg=  0 dialog_bg= 92 other=[] |  ┌ Question or follow-up ─────────────────────────────────────────────────────────────────┐
  row  7: input_bg= 87 dialog_bg=  4 other=[Yellow] |  │why did request latency spike at 12:00?                                                 │
  row  8: input_bg= 88 dialog_bg=  4 other=[] |  │                                                                                        │
  row  9: input_bg= 88 dialog_bg=  4 other=[] |  │                                                                                        │
  row 10: input_bg=  0 dialog_bg= 92 other=[] |  └────────────────────────────────────────────────────────────────────────────────────────┘
  row 11: input_bg=  0 dialog_bg= 92 other=[] |  ┌ State ─────────────────────────────────────────────────────────────────────────────────┐
  row 12: input_bg=  0 dialog_bg= 92 other=[] |  │Ready: enter a question for a new fixed snapshot                                        │
  row 13: input_bg=  0 dialog_bg= 92 other=[] |  └────────────────────────────────────────────────────────────────────────────────────────┘
  row 14: input_bg=  0 dialog_bg= 92 other=[] |  ┌ Activity and saved investigations ─────────────────────────────────────────────────────┐
  row 15: input_bg=  0 dialog_bg= 92 other=[] |  │Conversation:                                                                           │
  row 16: input_bg=  0 dialog_bg= 92 other=[] |  │activity 0: inspected the WARN burst and the worker restart window                      │
  row 17: input_bg=  0 dialog_bg= 92 other=[] |  │activity 1: inspected the WARN burst and the worker restart window                      │
  row 18: input_bg=  0 dialog_bg= 92 other=[] |  │activity 2: inspected the WARN burst and the worker restart window                      │
  row 19: input_bg=  0 dialog_bg= 92 other=[] |  │activity 3: inspected the WARN burst and the worker restart window                      │
  row 20: input_bg=  0 dialog_bg= 92 other=[] |  │activity 4: inspected the WARN burst and the worker restart window                      │
  row 21: input_bg=  0 dialog_bg= 92 other=[] |  │activity 5: inspected the WARN burst and the worker restart window                      │
  row 22: input_bg=  0 dialog_bg= 92 other=[] |  │                                                                                        │
  row 23: input_bg=  0 dialog_bg= 92 other=[] |  │                                                                                        │
  row 24: input_bg=  0 dialog_bg= 92 other=[] |  └────────────────────────────────────────────────────────────────────────────────────────┘
  row 25: input_bg=  0 dialog_bg= 92 other=[] | 

===== command-enrichment @ 100x30 modal=Rect { x: 8, y: 4, width: 84, height: 22 } =====
  row  4: input_bg=  0 dialog_bg= 84 other=[] |  Program: Executable path; no shell parsing
  row  5: input_bg= 81 dialog_bg=  2 other=[Yellow] |  /usr/bin/jq
  row  6: input_bg=  0 dialog_bg= 84 other=[] |  Arguments: 1 line(s) · One argument per line, e.g. --format then json
  row  7: input_bg= 82 dialog_bg=  2 other=[] |  -r '.trace_id'
  row  8: input_bg=  0 dialog_bg= 84 other=[] |  Working directory: Optional; defaults to this workspace directory
  row  9: input_bg= 82 dialog_bg=  2 other=[] |  /home/user/work
  row 10: input_bg=  0 dialog_bg= 84 other=[] |  Environment: 1 line(s) · Optional KEY=value per line, e.g. LANG=C
  row 11: input_bg= 82 dialog_bg=  2 other=[] |  TZ=UTC
  row 12: input_bg=  0 dialog_bg= 84 other=[] |  Applied command step: None · enrichment steps still apply
  row 13: input_bg=  0 dialog_bg= 84 other=[] | 
  row 14: input_bg=  0 dialog_bg= 84 other=[] |  [ New line (Alt-N) ] [ Save ] [ Review ] [ Remove ]
  row 15: input_bg=  0 dialog_bg= 84 other=[] | 
  row 16: input_bg=  0 dialog_bg= 84 other=[] |  ┌ Status and review ─────────────────────────────────────────────────────────────┐
  row 17: input_bg=  0 dialog_bg= 84 other=[] |  │Status: Unrun · Reviewed 16 sampled records; 3 produced no output and stay      │
  row 18: input_bg=  0 dialog_bg= 84 other=[] |  │unenriched.                                                                     │
  row 19: input_bg=  0 dialog_bg= 84 other=[] |  │Saving or restoring never starts this command. New records remain pending until │
  row 20: input_bg=  0 dialog_bg= 84 other=[] |  │another explicit run.                                                           │
  row 21: input_bg=  0 dialog_bg= 84 other=[] |  │Results appear in Details as command.<field>; command.status shows Ready or     │
  row 22: input_bg=  0 dialog_bg= 84 other=[] |  │Pending. Filters and field choices use the enrichment steps above.              │
  row 23: input_bg=  0 dialog_bg= 84 other=[] |  │                                                                                │
  row 24: input_bg=  0 dialog_bg= 84 other=[] |  │                                                                                │
  row 25: input_bg=  0 dialog_bg= 84 other=[] |  └────────────────────────────────────────────────────────────────────────────────┘
```
