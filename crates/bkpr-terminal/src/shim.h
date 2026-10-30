#ifndef BKPR_TERMINAL_SHIM_H
#define BKPR_TERMINAL_SHIM_H
#include <ghostty/vt.h>

/* Export the official header-only mode constant without duplicating its ABI. */
GhosttyMode bkpr_terminal_focus_mode(void);
GhosttyMode bkpr_terminal_sync_output_mode(void);
GhosttyMode bkpr_terminal_paste_events_mode(void);
#endif
