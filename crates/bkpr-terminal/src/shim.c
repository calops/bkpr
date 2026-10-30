#include "shim.h"

GhosttyMode bkpr_terminal_focus_mode(void) {
    return GHOSTTY_MODE_FOCUS_EVENT;
}

GhosttyMode bkpr_terminal_sync_output_mode(void) {
    return GHOSTTY_MODE_SYNC_OUTPUT;
}

GhosttyMode bkpr_terminal_paste_events_mode(void) {
    return GHOSTTY_MODE_PASTE_EVENTS;
}
