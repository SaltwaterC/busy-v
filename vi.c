#include "standalone.h"
// regex.h is already included by standalone.h.
// the CRASHME code is unmaintained, and doesn't currently build
// 0x9b is Meta-ESC
enum {
 MAX_TABSTOP = 32, // sanity limit
 // User input len. Need not be extra big.
 // Lines in file being edited *can* be bigger than this.
 MAX_INPUT_LEN = 128,
 // Sanity limits. We have only one buffer of this size.
 MAX_SCR_COLS = 4096,
 MAX_SCR_ROWS = 4096,
};
// VT102 ESC sequences.
// See "Xterm Control Sequences"
// http://invisible-island.net/xterm/ctlseqs/ctlseqs.html
// Inverse/Normal text
// Bell
// Clear-to-end-of-line
// Clear-to-end-of-screen.
// (We use default param here.
// Full sequence is "ESC [ <num> J",
// <num> is 0/1/2 = "erase below/above/all".)
// Cursor to given coordinate (1,1: top left)
//UNUSED
//// Cursor up and down
//#define ESC_CURSOR_UP   ESC"[A"
//#define ESC_CURSOR_DOWN "\n"
// cmds modifying text[]
static const char modifying_cmds[] __attribute__((aligned(1))) = "aAcCdDiIJoOpPrRs""xX<>~";
enum {
 YANKONLY = 0,
 YANKDEL = 1,
 FORWARD = 1, // code depends on "1"  for array index
 BACK = -1, // code depends on "-1" for array index
 LIMITED = 0, // char_search() only current line
 FULL = 1, // char_search() to the end/beginning of entire text
 PARTIAL = 0, // buffer contains partial line
 WHOLE = 1, // buffer contains whole lines
 MULTI = 2, // buffer may include newlines
 S_BEFORE_WS = 1, // used in skip_thing() for moving "dot"
 S_TO_WS = 2, // used in skip_thing() for moving "dot"
 S_OVER_WS = 3, // used in skip_thing() for moving "dot"
 S_END_PUNCT = 4, // used in skip_thing() for moving "dot"
 S_END_ALNUM = 5, // used in skip_thing() for moving "dot"
 C_END = -1, // cursor is at end of line due to '$' command
};
// vi.c expects chars to be unsigned.
// busybox build system provides that, but it's better
// to audit and fix the source
struct globals {
 // many references - keep near the top of globals
 char *text, *end; // pointers to the user data in memory
 char *dot; // where all the action takes place
 int text_size; // size of the allocated buffer
 // the rest
 signed char vi_setops; // set by setops()
// order of constants and strings must match
 signed char readonly_mode;
 signed char editing; // >0 while we are editing a file
                          // [code audit says "can be 0, 1 or 2 only"]
 signed char cmd_mode; // 0=command  1=insert 2=replace
 int modified_count; // buffer contents changed if !0
 int last_modified_count; // = -1;
 int cmdline_filecnt; // how many file names on cmd line
 int cmdcnt; // repetition count
 char *rstart; // start of text in Replace mode
 unsigned rows, columns; // the terminal screen is this size
 int get_rowcol_error;
 int crow, ccol; // cursor is on Crow x Ccol
 int offset; // chars scrolled off the screen to the left
 int have_status_msg; // is default edit status needed?
                          // [don't make smallint!]
 int last_status_cksum; // hash of current status line
 char *current_filename;
 char *alt_filename;
 char *screenbegin; // index into text[], of top line on the screen
 char *screen; // pointer to the virtual screen buffer
 int screensize; //            and its size
 int tabstop;
 int last_search_char; // last char searched for (int because of Unicode)
 signed char last_search_cmd; // command used to invoke last char search
 char undo_queue_state; // One of UNDO_INS, UNDO_DEL, UNDO_EMPTY
 signed char adding2q; // are we currently adding user input to q
 int lmc_len; // length of last_modifying_cmd
 char *ioq, *ioq_start; // pointer to string for get_one_char to "read"
 int dotcnt; // number of times to repeat '.' command
 char *last_search_pattern; // last pattern from a '/' or '?' search
 int char_insert__indentcol; // column of recent autoindent or 0
 int newindent; // autoindent value for 'O'/'cc' commands
      // or -1 to use indent from previous line
 signed char cmd_error;
 // former statics
 char *edit_file__cur_line;
 int refresh__old_offset;
 int format_edit_status__tot;
 // a few references only
 unsigned char YDreg;//,Ureg;// default delete register and orig line for "U"
 char *reg[28]; // named register a-z, "D", and "U" 0-25,26,27
 char regtype[28]; // buffer type: WHOLE, MULTI or PARTIAL
 char *mark[28]; // user marks points somewhere in text[]-  a-z and previous context ''
 sigjmp_buf restart; // int_handler() jumps to location remembered here
 struct termios term_orig; // remember what the cooked mode was
 int cindex; // saved character index for up/down motion
 signed char keep_index; // retain saved character index
 llist_t *initial_cmds;
 // Should be just enough to hold a key sequence,
 // but CRASHME mode uses it as generated command buffer too
 char readbuffer[16];
 char status_buffer[200]; // messages to the user
 char last_modifying_cmd[MAX_INPUT_LEN]; // last modifying cmd for "."
 char get_input_line__buf[MAX_INPUT_LEN]; // former static
 char scr_out_buf[MAX_SCR_COLS + MAX_TABSTOP * 2];
// undo_push() operations
// Pass-through flags for functions that can be undone
 struct undo_object {
  struct undo_object *prev; // Linking back avoids list traversal (LIFO)
  int start; // Offset where the data should be restored/deleted
  int length; // total data size
  uint8_t u_type; // 0=deleted, 1=inserted, 2=swapped
  char undo_text[1]; // text that was deleted (if deletion)
 } *undo_stack_tail;
 char *undo_queue_spos; // Start position of queued operation
 int undo_q;
 char undo_queue[256];
};
static struct globals *ptr_to_globals;
//#define Ureg           (G.Ureg          )
static void show_status_line(void); // put a message on the bottom line
static void show_help(void)
{
 puts("These features are available:"
 "\n\tPattern searches with / and ?"
 "\n\tLast command repeat with ."
 "\n\tLine marking with 'x"
 "\n\tNamed buffers with \"x"
 //not implemented: "\n\tReadonly if vi is called as \"view\""
 //redundant: usage text says this too: "\n\tReadonly with -R command line arg"
 "\n\tSome colon mode commands with :"
 "\n\tSettable options with \":set\""
 "\n\tSignal catching- ^C"
 "\n\tJob suspend and resume with ^Z"
 "\n\tAdapt to window re-sizes"
 );
}
static void write1(const char *out)
{
 fputs_stdout(out);
}
static int query_screen_dimensions(void)
{
 int err = get_terminal_width_height(0, &((*ptr_to_globals).columns ), &((*ptr_to_globals).rows ));
 if (((*ptr_to_globals).rows ) > MAX_SCR_ROWS)
  ((*ptr_to_globals).rows ) = MAX_SCR_ROWS;
 if (((*ptr_to_globals).columns ) > MAX_SCR_COLS)
  ((*ptr_to_globals).columns ) = MAX_SCR_COLS;
 return err;
}
// sleep for 'h' 1/100 seconds, return 1/0 if stdin is (ready for read)/(not ready)
static int mysleep(int hund)
{
 struct pollfd pfd[1];
 if (hund != 0)
  fflush_all();
 pfd[0].fd = 0;
 pfd[0].events = 0x001;
 return safe_poll(pfd, 1, hund*10) > 0;
}
//----- Set terminal attributes --------------------------------
static void rawmode(void)
{
 // no TERMIOS_CLEAR_ISIG: leave ISIG on - allow signals
 set_termios_to_raw(0, &((*ptr_to_globals).term_orig ), ((1 << 1) | (1 << 2)));
}
static void cookmode(void)
{
 fflush_all();
 tcsetattr_stdin_TCSANOW(&((*ptr_to_globals).term_orig ));
}
//----- Terminal Drawing ---------------------------------------
// The terminal is made up of 'rows' line of 'columns' columns.
// classically this would be 24 x 80.
//  screen coordinates
//  0,0     ...     0,79
//  1,0     ...     1,79
//  .       ...     .
//  .       ...     .
//  22,0    ...     22,79
//  23,0    ...     23,79   <- status line
//----- Move the cursor to row x col (count from 0, not 1) -------
static void place_cursor(int row, int col)
{
 char cm1[sizeof("\033""[%u;%uH") + sizeof(int)*3 * 2];
 if (row < 0) row = 0;
 if (row >= ((*ptr_to_globals).rows )) row = ((*ptr_to_globals).rows ) - 1;
 if (col < 0) col = 0;
 if (col >= ((*ptr_to_globals).columns )) col = ((*ptr_to_globals).columns ) - 1;
 sprintf(cm1, "\033""[%u;%uH", row + 1, col + 1);
 write1(cm1);
}
//----- Erase from cursor to end of line -----------------------
static void clear_to_eol(void)
{
 write1("\033""[K");
}
static void go_bottom_and_clear_to_eol(void)
{
 place_cursor(((*ptr_to_globals).rows ) - 1, 0);
 clear_to_eol();
}
//----- Start standout mode ------------------------------------
static void standout_start(void)
{
 write1("\033""[7m");
}
//----- End standout mode --------------------------------------
static void standout_end(void)
{
 write1("\033""[m");
}
//----- Text Movement Routines ---------------------------------
static char *begin_line(char *p) // return pointer to first char cur line
{
 if (p > ((*ptr_to_globals).text )) {
  p = memrchr(((*ptr_to_globals).text ), '\n', p - ((*ptr_to_globals).text ));
  if (!p)
   return ((*ptr_to_globals).text );
  return p + 1;
 }
 return p;
}
static char *end_line(char *p) // return pointer to NL of cur line
{
 if (p && p < ((*ptr_to_globals).end ) - 1) {
  p = memchr(p, '\n', ((*ptr_to_globals).end ) - p - 1);
  if (!p)
   return ((*ptr_to_globals).end ) - 1;
 }
 return p;
}
static char *dollar_line(char *p) // return pointer to just before NL line
{
 p = end_line(p);
 // Try to stay off of the Newline
 if (*p == '\n' && (p - begin_line(p)) > 0)
  p--;
 return p;
}
static char *prev_line(char *p) // return pointer first char prev line
{
 p = begin_line(p); // goto beginning of cur line
 if (p > ((*ptr_to_globals).text ) && p[-1] == '\n')
  p--; // step to prev line
 p = begin_line(p); // goto beginning of prev line
 return p;
}
static char *next_line(char *p) // return pointer first char next line
{
 p = end_line(p);
 if (p < ((*ptr_to_globals).end ) - 1 && *p == '\n')
  p++; // step to next line
 return p;
}
//----- Text Information Routines ------------------------------
static char *end_screen(void)
{
 char *q;
 int cnt;
 // find new bottom line
 q = ((*ptr_to_globals).screenbegin );
 for (cnt = 0; cnt < ((*ptr_to_globals).rows ) - 2; cnt++)
  q = next_line(q);
 q = end_line(q);
 return q;
}
// count line from start to stop
static int count_lines(char *start, char *stop)
{
 char *q;
 int cnt;
 if (stop < start) { // start and stop are backwards- reverse them
  q = start;
  start = stop;
  stop = q;
 }
 cnt = 0;
 stop = end_line(stop);
 while (start <= stop && start <= ((*ptr_to_globals).end ) - 1) {
  start = end_line(start);
  if (*start == '\n')
   cnt++;
  start++;
 }
 return cnt;
}
static char *find_line(int li) // find beginning of line #li
{
 char *q;
 for (q = ((*ptr_to_globals).text ); li > 1; li--) {
  q = next_line(q);
 }
 return q;
}
static int next_tabstop(int col)
{
 return col + ((((*ptr_to_globals).tabstop ) - 1) - (col % ((*ptr_to_globals).tabstop )));
}
static int prev_tabstop(int col)
{
 return col - ((col % ((*ptr_to_globals).tabstop )) ?: ((*ptr_to_globals).tabstop ));
}
static int next_column(char c, int co)
{
 if (c == '\t')
  co = next_tabstop(co);
 else if ((unsigned char)c < ' ' || c == 0x7f)
  co++; // display as ^X, use 2 columns
 return co + 1;
}
static int get_column(char *p)
{
 const char *r;
 int co = 0;
 for (r = begin_line(p); r < p; r++)
  co = next_column(*r, co);
 return co;
}
//----- Erase the Screen[] memory ------------------------------
static void screen_erase(void)
{
 memset(((*ptr_to_globals).screen ), ' ', ((*ptr_to_globals).screensize )); // clear new screen
}
static void new_screen(int ro, int co)
{
 char *s;
 free(((*ptr_to_globals).screen ));
 ((*ptr_to_globals).screensize ) = ro * co + 8;
 s = ((*ptr_to_globals).screen ) = xmalloc(((*ptr_to_globals).screensize ));
 // initialize the new screen. assume this will be a empty file.
 screen_erase();
 // non-existent text[] lines start with a tilde (~).
 //screen[(1 * co) + 0] = '~';
 //screen[(2 * co) + 0] = '~';
 //..
 //screen[((ro-2) * co) + 0] = '~';
 ro -= 2;
 while (--ro >= 0) {
  s += co;
  *s = '~';
 }
}
//----- Synchronize the cursor to Dot --------------------------
static void sync_cursor(char *d, int *row, int *col)
{
 char *beg_cur; // begin and end of "d" line
 char *tp;
 int cnt, ro, co;
 beg_cur = begin_line(d); // first char of cur line
 if (beg_cur < ((*ptr_to_globals).screenbegin )) {
  // "d" is before top line on screen
  // how many lines do we have to move
  cnt = count_lines(beg_cur, ((*ptr_to_globals).screenbegin ));
 sc1:
  ((*ptr_to_globals).screenbegin ) = beg_cur;
  if (cnt > (((*ptr_to_globals).rows ) - 1) / 2) {
   // we moved too many lines. put "dot" in middle of screen
   for (cnt = 0; cnt < (((*ptr_to_globals).rows ) - 1) / 2; cnt++) {
    ((*ptr_to_globals).screenbegin ) = prev_line(((*ptr_to_globals).screenbegin ));
   }
  }
 } else {
  char *end_scr; // begin and end of screen
  end_scr = end_screen(); // last char of screen
  if (beg_cur > end_scr) {
   // "d" is after bottom line on screen
   // how many lines do we have to move
   cnt = count_lines(end_scr, beg_cur);
   if (cnt > (((*ptr_to_globals).rows ) - 1) / 2)
    goto sc1; // too many lines
   for (ro = 0; ro < cnt - 1; ro++) {
    // move screen begin the same amount
    ((*ptr_to_globals).screenbegin ) = next_line(((*ptr_to_globals).screenbegin ));
    // now, move the end of screen
    end_scr = next_line(end_scr);
    end_scr = end_line(end_scr);
   }
  }
 }
 // "d" is on screen- find out which row
 tp = ((*ptr_to_globals).screenbegin );
 for (ro = 0; ro < ((*ptr_to_globals).rows ) - 1; ro++) { // drive "ro" to correct row
  if (tp == beg_cur)
   break;
  tp = next_line(tp);
 }
 // find out what col "d" is on
 co = 0;
 do { // drive "co" to correct column
  if (*tp == '\n') //vda || *tp == '\0')
   break;
  co = next_column(*tp, co) - 1;
  // inserting text before a tab, don't include its position
  if (((*ptr_to_globals).cmd_mode ) && tp == d - 1 && *d == '\t') {
   co++;
   break;
  }
 } while (tp++ < d && ++co);
 // "co" is the column where "dot" is.
 // The screen has "columns" columns.
 // The currently displayed columns are  0+offset -- columns+ofset
 // |-------------------------------------------------------------|
 //               ^ ^                                ^
 //        offset | |------- columns ----------------|
 //
 // If "co" is already in this range then we do not have to adjust offset
 //      but, we do have to subtract the "offset" bias from "co".
 // If "co" is outside this range then we have to change "offset".
 // If the first char of a line is a tab the cursor will try to stay
 //  in column 7, but we have to set offset to 0.
 if (co < 0 + ((*ptr_to_globals).offset )) {
  ((*ptr_to_globals).offset ) = co;
 }
 if (co >= ((*ptr_to_globals).columns ) + ((*ptr_to_globals).offset )) {
  ((*ptr_to_globals).offset ) = co - ((*ptr_to_globals).columns ) + 1;
 }
 // if the first char of the line is a tab, and "dot" is sitting on it
 //  force offset to 0.
 if (d == beg_cur && *d == '\t') {
  ((*ptr_to_globals).offset ) = 0;
 }
 co -= ((*ptr_to_globals).offset );
 *row = ro;
 *col = co;
}
//----- Format a text[] line into a buffer ---------------------
static char* format_line(char *src /*, int li*/)
{
 unsigned char c;
 int co;
 int ofs = ((*ptr_to_globals).offset );
 char *dest = ((*ptr_to_globals).scr_out_buf ); // [MAX_SCR_COLS + MAX_TABSTOP * 2]
 c = '~'; // char in col 0 in non-existent lines is '~'
 co = 0;
 while (co < ((*ptr_to_globals).columns ) + ((*ptr_to_globals).tabstop )) {
  // have we gone past the end?
  if (src < ((*ptr_to_globals).end )) {
   c = *src++;
   if (c == '\n')
    break;
   if ((c & 0x80) && !((unsigned char)(c) >= ' ' && (c) != 0x7f && (unsigned char)(c) != 0x9b)) {
    c = '.';
   }
   if (c < ' ' || c == 0x7f) {
    if (c == '\t') {
     c = ' ';
     //      co %    8     !=     7
     while ((co % ((*ptr_to_globals).tabstop )) != (((*ptr_to_globals).tabstop ) - 1)) {
      dest[co++] = c;
     }
    } else {
     dest[co++] = '^';
     if (c == 0x7f)
      c = '?';
     else
      c += '@'; // Ctrl-X -> 'X'
    }
   }
  }
  dest[co++] = c;
  // discard scrolled-off-to-the-left portion,
  // in tabstop-sized pieces
  if (ofs >= ((*ptr_to_globals).tabstop ) && co >= ((*ptr_to_globals).tabstop )) {
   memmove(dest, dest + ((*ptr_to_globals).tabstop ), co);
   co -= ((*ptr_to_globals).tabstop );
   ofs -= ((*ptr_to_globals).tabstop );
  }
  if (src >= ((*ptr_to_globals).end ))
   break;
 }
 // check "short line, gigantic offset" case
 if (co < ofs)
  ofs = co;
 // discard last scrolled off part
 co -= ofs;
 dest += ofs;
 // fill the rest with spaces
 if (co < ((*ptr_to_globals).columns ))
  memset(&dest[co], ' ', ((*ptr_to_globals).columns ) - co);
 return dest;
}
//----- Refresh the changed screen lines -----------------------
// Copy the source line from text[] into the buffer and note
// if the current screenline is different from the new buffer.
// If they differ then that line needs redrawing on the terminal.
//
static void refresh(int full_screen)
{
 int li, changed;
 char *tp, *sp; // pointer into text[] and screen[]
 if (1 && !(*ptr_to_globals).get_rowcol_error ) {
  unsigned c = ((*ptr_to_globals).columns ), r = ((*ptr_to_globals).rows );
  query_screen_dimensions();
  full_screen |= (c - ((*ptr_to_globals).columns )) | (r - ((*ptr_to_globals).rows ));
 }
 sync_cursor(((*ptr_to_globals).dot ), &((*ptr_to_globals).crow ), &((*ptr_to_globals).ccol )); // where cursor will be (on "dot")
 tp = ((*ptr_to_globals).screenbegin ); // index into text[] of top line
 // compare text[] to screen[] and mark screen[] lines that need updating
 for (li = 0; li < ((*ptr_to_globals).rows ) - 1; li++) {
  int cs, ce; // column start & end
  char *out_buf;
  // format current text line
  out_buf = format_line(tp /*, li*/);
  // skip to the end of the current text[] line
  if (tp < ((*ptr_to_globals).end )) {
   char *t = memchr(tp, '\n', ((*ptr_to_globals).end ) - tp);
   if (!t) t = ((*ptr_to_globals).end ) - 1;
   tp = t + 1;
  }
  // see if there are any changes between virtual screen and out_buf
  changed = 0; // assume no change
  cs = 0;
  ce = ((*ptr_to_globals).columns ) - 1;
  sp = &((*ptr_to_globals).screen )[li * ((*ptr_to_globals).columns )]; // start of screen line
  if (full_screen) {
   // force re-draw of every single column from 0 - columns-1
   goto re0;
  }
  // compare newly formatted buffer with virtual screen
  // look forward for first difference between buf and screen
  for (; cs <= ce; cs++) {
   if (out_buf[cs] != sp[cs]) {
    changed = 1; // mark for redraw
    break;
   }
  }
  // look backward for last difference between out_buf and screen
  for (; ce >= cs; ce--) {
   if (out_buf[ce] != sp[ce]) {
    changed = 1; // mark for redraw
    break;
   }
  }
  // now, cs is index of first diff, and ce is index of last diff
  // if horz offset has changed, force a redraw
  if (((*ptr_to_globals).offset ) != ((*ptr_to_globals).refresh__old_offset)) {
 re0:
   changed = 1;
  }
  // make a sanity check of columns indexes
  if (cs < 0) cs = 0;
  if (ce > ((*ptr_to_globals).columns ) - 1) ce = ((*ptr_to_globals).columns ) - 1;
  if (cs > ce) { cs = 0; ce = ((*ptr_to_globals).columns ) - 1; }
  // is there a change between virtual screen and out_buf
  if (changed) {
   // copy changed part of buffer to virtual screen
   memcpy(sp+cs, out_buf+cs, ce-cs+1);
   place_cursor(li, cs);
   // write line out to terminal
   fwrite(&sp[cs], ce - cs + 1, 1, stdout);
  }
 }
 place_cursor(((*ptr_to_globals).crow ), ((*ptr_to_globals).ccol ));
 if (!((*ptr_to_globals).keep_index ))
  ((*ptr_to_globals).cindex ) = ((*ptr_to_globals).ccol ) + ((*ptr_to_globals).offset );
 ((*ptr_to_globals).refresh__old_offset) = ((*ptr_to_globals).offset );
}
//----- Force refresh of all Lines -----------------------------
static void redraw(int full_screen)
{
 // cursor to top,left; clear to the end of screen
 write1("\033""[H" "\033""[J");
 screen_erase(); // erase the internal screen buffer
 ((*ptr_to_globals).last_status_cksum ) = 0; // force status update
 refresh(full_screen); // this will redraw the entire display
 show_status_line();
}
//----- Flash the screen  --------------------------------------
static void flash(int h)
{
 //standout_start();
 //redraw(TRUE);
 write1("\033""[?5h"); // "reverse screen on"
 mysleep(h);
 //standout_end();
 //redraw(TRUE);
 write1("\033""[?5l"); // "reverse screen off"
}
static void indicate_error(void)
{
 ((*ptr_to_globals).cmd_error ) = 1;
 if (!(((*ptr_to_globals).vi_setops ) & (1 << 2))) {
  write1("\007");
 } else {
  flash(10);
 }
}
//----- IO Routines --------------------------------------------
static int readit(void) // read (maybe cursor) key from stdin
{
 int c;
 fflush_all();
 // Wait for input. TIMEOUT = -1 makes read_key wait even
 // on nonblocking stdin.
 // Note: read_key sets errno to 0 on success.
 again:
 c = safe_read_key(0, ((*ptr_to_globals).readbuffer ), /*timeout:*/ -1);
 if (c == -1) { // EOF/error
  if ((*__errno_location ()) == 11) // paranoia
   goto again;
  go_bottom_and_clear_to_eol();
  cookmode(); // terminal to "cooked"
  vi_error_and_die("can't read user input");
 }
 return c;
}
static int get_one_char(void)
{
 int c;
 if (!((*ptr_to_globals).adding2q )) {
  // we are not adding to the q.
  // but, we may be reading from a saved q.
  // (checking "ioq" for NULL is wrong, it's not reset to NULL
  // when done - "ioq_start" is reset instead).
  if (((*ptr_to_globals).ioq_start ) != ((void*)0)) {
   // there is a queue to get chars from.
   // careful with correct sign expansion!
   c = (unsigned char)*((*ptr_to_globals).ioq )++;
   if (c != '\0')
    return c;
   // the end of the q
   free(((*ptr_to_globals).ioq_start ));
   ((*ptr_to_globals).ioq_start ) = ((void*)0);
   // read from STDIN:
  }
  return readit();
 }
 // we are adding STDIN chars to q.
 c = readit();
 if (((*ptr_to_globals).lmc_len ) >= (sizeof(((*ptr_to_globals).last_modifying_cmd )) / sizeof((((*ptr_to_globals).last_modifying_cmd ))[0])) - 2) {
  // last_modifying_cmd[] is too small, can't remember the cmd
  // - drop it
  ((*ptr_to_globals).adding2q ) = 0;
  ((*ptr_to_globals).lmc_len ) = 0;
 } else {
  ((*ptr_to_globals).last_modifying_cmd )[((*ptr_to_globals).lmc_len )++] = c;
 }
 return c;
}
// Get type of thing to operate on and adjust count
static int get_motion_char(void)
{
 int c, cnt;
 c = get_one_char();
 if (((*__ctype_b_loc ())[(int) ((c))] & (unsigned short int) _ISdigit)) {
  if (c != '0') {
   // get any non-zero motion count
   for (cnt = 0; ((*__ctype_b_loc ())[(int) ((c))] & (unsigned short int) _ISdigit); c = get_one_char())
    cnt = cnt * 10 + (c - '0');
   ((*ptr_to_globals).cmdcnt ) = (((*ptr_to_globals).cmdcnt ) ?: 1) * cnt;
  } else {
   // ensure standalone '0' works
   ((*ptr_to_globals).cmdcnt ) = 0;
  }
 }
 return c;
}
// Get input line (uses "status line" area)
static char *get_input_line(const char *prompt)
{
 // char [MAX_INPUT_LEN]
 int c;
 int i;
 strcpy(((*ptr_to_globals).get_input_line__buf), prompt);
 ((*ptr_to_globals).last_status_cksum ) = 0; // force status update
 go_bottom_and_clear_to_eol();
 write1(((*ptr_to_globals).get_input_line__buf)); // write out the :, /, or ? prompt
 i = strlen(((*ptr_to_globals).get_input_line__buf));
 while (i < MAX_INPUT_LEN - 1) {
  c = get_one_char();
  if (c == '\n' || c == '\r' || c == 27)
   break; // this is end of input
  if (((c) == ((*ptr_to_globals).term_orig ).c_cc[2] || (c) == 8 || (c) == 127)) {
   // user wants to erase prev char
   ((*ptr_to_globals).get_input_line__buf)[--i] = '\0';
   go_bottom_and_clear_to_eol();
   if (i <= 0) // user backs up before b-o-l, exit
    break;
   write1(((*ptr_to_globals).get_input_line__buf));
  } else if (c > 0 && c < 256) { // exclude Unicode
   // (TODO: need to handle Unicode)
   ((*ptr_to_globals).get_input_line__buf)[i] = c;
   ((*ptr_to_globals).get_input_line__buf)[++i] = '\0';
  vi_putchar(c);
  }
 }
 refresh(0);
 return ((*ptr_to_globals).get_input_line__buf);
}
static void Hit_Return(void)
{
 int c;
 standout_start();
 write1("[Hit return to continue]");
 standout_end();
 while ((c = get_one_char()) != '\n' && c != '\r')
  continue;
 redraw(1); // force redraw all
}
//----- Draw the status line at bottom of the screen -------------
// show file status on status line
static int format_edit_status(void)
{
 static const char cmd_mode_indicator[] __attribute__((aligned(1))) = "-IR-";
 int cur, percent, ret, trunc_at;
 // modified_count is now a counter rather than a flag.  this
 // helps reduce the amount of line counting we need to do.
 // (this will cause a mis-reporting of modified status
 // once every MAXINT editing operations.)
 // it would be nice to do a similar optimization here -- if
 // we haven't done a motion that could have changed which line
 // we're on, then we shouldn't have to do this count_lines()
 cur = count_lines(((*ptr_to_globals).text ), ((*ptr_to_globals).dot ));
 // count_lines() is expensive.
 // Call it only if something was changed since last time
 // we were here:
 if (((*ptr_to_globals).modified_count ) != ((*ptr_to_globals).last_modified_count)) {
  ((*ptr_to_globals).format_edit_status__tot) = cur + count_lines(((*ptr_to_globals).dot ), ((*ptr_to_globals).end ) - 1) - 1;
  ((*ptr_to_globals).last_modified_count) = ((*ptr_to_globals).modified_count );
 }
 //    current line         percent
 //   -------------    ~~ ----------
 //    total lines            100
 if (((*ptr_to_globals).format_edit_status__tot) > 0) {
  percent = (100 * cur) / ((*ptr_to_globals).format_edit_status__tot);
 } else {
  cur = ((*ptr_to_globals).format_edit_status__tot) = 0;
  percent = 100;
 }
 trunc_at = ((*ptr_to_globals).columns ) < 200 -1 ?
  ((*ptr_to_globals).columns ) : 200 -1;
 ret = snprintf(((*ptr_to_globals).status_buffer ), trunc_at+1,
  "%c %s%s%s %d/%d %d%%",
  cmd_mode_indicator[((*ptr_to_globals).cmd_mode ) & 3],
  (((*ptr_to_globals).current_filename ) != ((void*)0) ? ((*ptr_to_globals).current_filename ) : "No file"),
  (((*ptr_to_globals).readonly_mode ) ? " [Readonly]" : ""),
  (((*ptr_to_globals).modified_count ) ? " [Modified]" : ""),
  cur, ((*ptr_to_globals).format_edit_status__tot), percent);
 if (ret >= 0 && ret < trunc_at)
  return ret; // it all fit
 return trunc_at; // had to truncate
}
static int bufsum(char *buf, int count)
{
 int sum = 0;
 char *e = buf + count;
 while (buf < e)
  sum += (unsigned char) *buf++;
 return sum;
}
static void show_status_line(void)
{
 int cnt = 0, cksum = 0;
 // either we already have an error or status message, or we
 // create one.
 if (!((*ptr_to_globals).have_status_msg )) {
  cnt = format_edit_status();
  cksum = bufsum(((*ptr_to_globals).status_buffer ), cnt);
 }
 if (((*ptr_to_globals).have_status_msg ) || ((cnt > 0 && ((*ptr_to_globals).last_status_cksum ) != cksum))) {
  ((*ptr_to_globals).last_status_cksum ) = cksum; // remember if we have seen this line
  go_bottom_and_clear_to_eol();
  write1(((*ptr_to_globals).status_buffer ));
  if (((*ptr_to_globals).have_status_msg )) {
   int n = (int)strlen(((*ptr_to_globals).status_buffer )) - (((*ptr_to_globals).have_status_msg ) - 1);
   // careful with int->unsigned promotion in comparison!
   if (n >= 0 && n >= ((*ptr_to_globals).columns ))
    Hit_Return();
   ((*ptr_to_globals).have_status_msg ) = 0;
  }
  place_cursor(((*ptr_to_globals).crow ), ((*ptr_to_globals).ccol )); // put cursor back in correct place
 }
 fflush_all();
}
//----- format the status buffer, the bottom line of screen ------
static void status_line(const char *format, ...)
{
 va_list args;
 __builtin_va_start(args, format);
 vsnprintf(((*ptr_to_globals).status_buffer ), 200, format, args);
 __builtin_va_end(args);
 ((*ptr_to_globals).have_status_msg ) = 1;
}
static void status_line_bold(const char *format, ...)
{
 va_list args;
 __builtin_va_start(args, format);
 strcpy(((*ptr_to_globals).status_buffer ), "\033""[7m");
 vsnprintf(((*ptr_to_globals).status_buffer ) + (sizeof("\033""[7m")-1),
  200 - sizeof("\033""[7m") - sizeof("\033""[m"),
  format, args
 );
 strcat(((*ptr_to_globals).status_buffer ), "\033""[m");
 __builtin_va_end(args);
 ((*ptr_to_globals).have_status_msg ) = 1 + (sizeof("\033""[7m")-1) + (sizeof("\033""[m")-1);
}
static void status_line_bold_errno(const char *fn)
{
 status_line_bold("'%s' ""%s", fn , strerror((*__errno_location ())));
}
// copy s to buf, convert unprintable
static void print_literal(char *buf, const char *s)
{
 char *d;
 unsigned char c;
 if (!s[0])
  s = "(NULL)";
 d = buf;
 for (; *s; s++) {
  c = *s;
  if ((c & 0x80) && !((unsigned char)(c) >= ' ' && (c) != 0x7f && (unsigned char)(c) != 0x9b))
   c = '?';
  if (c < ' ' || c == 0x7f) {
   *d++ = '^';
   c |= '@'; // 0x40
   if (c == 0x7f)
    c = '?';
  }
  *d++ = c;
  *d = '\0';
  if (d - buf > MAX_INPUT_LEN - 10) // paranoia
   break;
 }
}
static void not_implemented(const char *s)
{
 char buf[MAX_INPUT_LEN];
 print_literal(buf, s);
 status_line_bold("'%s' is not implemented", buf);
}
//----- Block insert/delete, undo ops --------------------------
// copy text into a register
static char *text_yank(char *p, char *q, int dest, int buftype)
{
 char *oldreg = ((*ptr_to_globals).reg )[dest];
 int cnt = q - p;
 if (cnt < 0) { // they are backwards- reverse them
  p = q;
  cnt = -cnt;
 }
 // Don't free register yet.  This prevents the memory allocator
 // from reusing the free block so we can detect if it's changed.
 ((*ptr_to_globals).reg )[dest] = xstrndup(p, cnt + 1);
 ((*ptr_to_globals).regtype )[dest] = buftype;
 free(oldreg);
 return p;
}
static char what_reg(void)
{
 char c;
 c = 'D'; // default to D-reg
 if (((*ptr_to_globals).YDreg ) <= 25)
  c = 'a' + (char) ((*ptr_to_globals).YDreg );
 if (((*ptr_to_globals).YDreg ) == 26)
  c = 'D';
 if (((*ptr_to_globals).YDreg ) == 27)
  c = 'U';
 return c;
}
static void check_context(char cmd)
{
 // Certain movement commands update the context.
 if (strchr(":%{}'GHLMz/?Nn", cmd) != ((void*)0)) {
  ((*ptr_to_globals).mark )[27] = ((*ptr_to_globals).mark )[26]; // move cur to prev
  ((*ptr_to_globals).mark )[26] = ((*ptr_to_globals).dot ); // move local to cur
 }
}
static char *swap_context(char *p) // goto new context for '' command make this the current context
{
 char *tmp;
 // the current context is in mark[26]
 // the previous context is in mark[27]
 // only swap context if other context is valid
 if (((*ptr_to_globals).text ) <= ((*ptr_to_globals).mark )[27] && ((*ptr_to_globals).mark )[27] <= ((*ptr_to_globals).end ) - 1) {
  tmp = ((*ptr_to_globals).mark )[27];
  ((*ptr_to_globals).mark )[27] = p;
  ((*ptr_to_globals).mark )[26] = p = tmp;
 }
 return p;
}
static void yank_status(const char *op, const char *p, int cnt)
{
 int lines, chars;
 lines = chars = 0;
 while (*p) {
  ++chars;
  if (*p++ == '\n')
   ++lines;
 }
 status_line("%s %d lines (%d chars) from [%c]",
    op, lines * cnt, chars * cnt, what_reg());
}
static void undo_push(char *, unsigned, int);
// open a hole in text[]
// might reallocate text[]! use p += text_hole_make(p, ...),
// and be careful to not use pointers into potentially freed text[]!
static uintptr_t text_hole_make(char *p, int size) // at "p", make a 'size' byte hole
{
 uintptr_t bias = 0;
 if (size <= 0)
  return bias;
 ((*ptr_to_globals).end ) += size; // adjust the new END
 if (((*ptr_to_globals).end ) >= (((*ptr_to_globals).text ) + ((*ptr_to_globals).text_size ))) {
  char *new_text;
  ((*ptr_to_globals).text_size ) += ((*ptr_to_globals).end ) - (((*ptr_to_globals).text ) + ((*ptr_to_globals).text_size )) + 10240;
  new_text = xrealloc(((*ptr_to_globals).text ), ((*ptr_to_globals).text_size ));
  bias = (new_text - ((*ptr_to_globals).text ));
  ((*ptr_to_globals).screenbegin ) += bias;
  ((*ptr_to_globals).dot ) += bias;
  ((*ptr_to_globals).end ) += bias;
  p += bias;
  {
   int i;
   for (i = 0; i < (sizeof(((*ptr_to_globals).mark )) / sizeof((((*ptr_to_globals).mark ))[0])); i++)
    if (((*ptr_to_globals).mark )[i])
     ((*ptr_to_globals).mark )[i] += bias;
  }
  ((*ptr_to_globals).text ) = new_text;
 }
 memmove(p + size, p, ((*ptr_to_globals).end ) - size - p);
 memset(p, ' ', size); // clear new hole
 return bias;
}
// close a hole in text[] - delete "p" through "q", inclusive
// "undo" value indicates if this operation should be undo-able
static char *text_hole_delete(char *p, char *q, int undo)
{
 char *src, *dest;
 int cnt, hole_size;
 // move forwards, from beginning
 // assume p <= q
 src = q + 1;
 dest = p;
 if (q < p) { // they are backward- swap them
  src = p + 1;
  dest = q;
 }
 hole_size = q - p + 1;
 cnt = ((*ptr_to_globals).end ) - src;
 switch (undo) {
  case 0:
   break;
  case 1:
   undo_push(p, hole_size, 1);
   break;
  case 2:
   undo_push(p, hole_size, 3);
   break;
  case 3:
   undo_push(p, hole_size, 5);
   break;
 }
 ((*ptr_to_globals).modified_count )--;
 if (src < ((*ptr_to_globals).text ) || src > ((*ptr_to_globals).end ))
  goto thd0;
 if (dest < ((*ptr_to_globals).text ) || dest >= ((*ptr_to_globals).end ))
  goto thd0;
 ((*ptr_to_globals).modified_count )++;
 if (src >= ((*ptr_to_globals).end ))
  goto thd_atend; // just delete the end of the buffer
 memmove(dest, src, cnt);
 thd_atend:
 ((*ptr_to_globals).end ) = ((*ptr_to_globals).end ) - hole_size; // adjust the new END
 if (dest >= ((*ptr_to_globals).end ))
  dest = ((*ptr_to_globals).end ) - 1; // make sure dest in below end-1
 if (((*ptr_to_globals).end ) <= ((*ptr_to_globals).text ))
  dest = ((*ptr_to_globals).end ) = ((*ptr_to_globals).text ); // keep pointers valid
 thd0:
 return dest;
}
// Flush any queued objects to the undo stack
static void undo_queue_commit(void)
{
 // Pushes the queue object onto the undo stack
 if (((*ptr_to_globals).undo_q ) > 0) {
  // Deleted character undo events grow from the end
  undo_push(((*ptr_to_globals).undo_queue ) + 256 - ((*ptr_to_globals).undo_q ),
   ((*ptr_to_globals).undo_q ),
   (((*ptr_to_globals).undo_queue_state) | 32)
  );
  ((*ptr_to_globals).undo_queue_state) = 64;
  ((*ptr_to_globals).undo_q ) = 0;
 }
}
static void flush_undo_data(void)
{
 struct undo_object *undo_entry;
 while (((*ptr_to_globals).undo_stack_tail )) {
  undo_entry = ((*ptr_to_globals).undo_stack_tail );
  ((*ptr_to_globals).undo_stack_tail ) = undo_entry->prev;
  free(undo_entry);
 }
}
// Undo functions and hooks added by Jody Bruchon (jody@jodybruchon.com)
// Add to the undo stack
static void undo_push(char *src, unsigned length, int u_type)
{
 struct undo_object *undo_entry;
 int use_spos = u_type & 32;
 // "u_type" values
 // UNDO_INS: insertion, undo will remove from buffer
 // UNDO_DEL: deleted text, undo will restore to buffer
 // UNDO_{INS,DEL}_CHAIN: Same as above but also calls undo_pop() when complete
 // The CHAIN operations are for handling multiple operations that the user
 // performs with a single action, i.e. REPLACE mode or find-and-replace commands
 // UNDO_{INS,DEL}_QUEUED: If queuing feature is enabled, allow use of the queue
 // for the INS/DEL operation.
 // UNDO_{INS,DEL} ORed with UNDO_USE_SPOS: commit the undo queue
 // This undo queuing functionality groups multiple character typing or backspaces
 // into a single large undo object. This greatly reduces calls to malloc() for
 // single-character operations while typing and has the side benefit of letting
 // an undo operation remove chunks of text rather than a single character.
 switch (u_type) {
 case 64: // Just in case this ever happens...
  return;
 case 5:
  if (length != 1)
   return; // Only queue single characters
  switch (((*ptr_to_globals).undo_queue_state)) {
  case 64:
   ((*ptr_to_globals).undo_queue_state) = 1;
  case 1:
   ((*ptr_to_globals).undo_queue_spos ) = src;
   ((*ptr_to_globals).undo_q )++;
   ((*ptr_to_globals).undo_queue )[256 - ((*ptr_to_globals).undo_q )] = *src;
   // If queue is full, dump it into an object
   if (((*ptr_to_globals).undo_q ) == 256)
    undo_queue_commit();
   return;
  case 0:
   // Switch from storing inserted text to deleted text
   undo_queue_commit();
   undo_push(src, length, 5);
   return;
  }
  break;
 case 4:
  if (length < 1)
   return;
  switch (((*ptr_to_globals).undo_queue_state)) {
  case 64:
   ((*ptr_to_globals).undo_queue_state) = 0;
   ((*ptr_to_globals).undo_queue_spos ) = src;
  case 0:
   while (length--) {
    ((*ptr_to_globals).undo_q )++; // Don't need to save any data for insertions
    if (((*ptr_to_globals).undo_q ) == 256)
     undo_queue_commit();
   }
   return;
  case 1:
   // Switch from storing deleted text to inserted text
   undo_queue_commit();
   undo_push(src, length, 4);
   return;
  }
  break;
 }
 u_type &= ~32;
 // Allocate a new undo object
 if (u_type == 1 || u_type == 3) {
  // For UNDO_DEL objects, save deleted text
  if ((((*ptr_to_globals).text ) + length) == ((*ptr_to_globals).end ))
   length--;
  // If this deletion empties text[], strip the newline. When the buffer becomes
  // zero-length, a newline is added back, which requires this to compensate.
  undo_entry = xzalloc(__builtin_offsetof(struct undo_object, undo_text) + length);
  memcpy(undo_entry->undo_text, src, length);
 } else {
  undo_entry = xzalloc(sizeof(*undo_entry));
 }
 undo_entry->length = length;
 if (use_spos) {
  undo_entry->start = ((*ptr_to_globals).undo_queue_spos ) - ((*ptr_to_globals).text ); // use start position from queue
 } else {
  undo_entry->start = src - ((*ptr_to_globals).text ); // use offset from start of text buffer
 }
 undo_entry->u_type = u_type;
 // Push it on undo stack
 undo_entry->prev = ((*ptr_to_globals).undo_stack_tail );
 ((*ptr_to_globals).undo_stack_tail ) = undo_entry;
 ((*ptr_to_globals).modified_count )++;
}
static void undo_push_insert(char *p, int len, int undo)
{
 switch (undo) {
 case 1:
  undo_push(p, len, 0);
  break;
 case 2:
  undo_push(p, len, 2);
  break;
 case 3:
  undo_push(p, len, 4);
  break;
 }
}
// Undo the last operation
static void undo_pop(void)
{
 int repeat;
 char *u_start, *u_end;
 struct undo_object *undo_entry;
 // Commit pending undo queue before popping (should be unnecessary)
 undo_queue_commit();
 undo_entry = ((*ptr_to_globals).undo_stack_tail );
 // Check for an empty undo stack
 if (!undo_entry) {
  status_line("Already at oldest change");
  return;
 }
 switch (undo_entry->u_type) {
 case 1:
 case 3:
  // make hole and put in text that was deleted; deallocate text
  u_start = ((*ptr_to_globals).text ) + undo_entry->start;
  text_hole_make(u_start, undo_entry->length);
  memcpy(u_start, undo_entry->undo_text, undo_entry->length);
  status_line("Undo [%d] %s %d chars at position %d",
   ((*ptr_to_globals).modified_count ), "restored",
   undo_entry->length, undo_entry->start
  );
  break;
 case 0:
 case 2:
  // delete what was inserted
  u_start = undo_entry->start + ((*ptr_to_globals).text );
  u_end = u_start - 1 + undo_entry->length;
  text_hole_delete(u_start, u_end, 0);
  status_line("Undo [%d] %s %d chars at position %d",
   ((*ptr_to_globals).modified_count ), "deleted",
   undo_entry->length, undo_entry->start
  );
  break;
 }
 repeat = 0;
 switch (undo_entry->u_type) {
 // If this is the end of a chain, lower modification count and refresh display
 case 1:
 case 0:
  ((*ptr_to_globals).dot ) = (((*ptr_to_globals).text ) + undo_entry->start);
  refresh(0);
  break;
 case 3:
 case 2:
  repeat = 1;
  break;
 }
 // Deallocate the undo object we just processed
 ((*ptr_to_globals).undo_stack_tail ) = undo_entry->prev;
 free(undo_entry);
 ((*ptr_to_globals).modified_count )--;
 // For chained operations, continue popping all the way down the chain.
 if (repeat) {
  undo_pop(); // Follow the undo chain if one exists
 }
}
//----- Dot Movement Routines ----------------------------------
static void dot_left(void)
{
 undo_queue_commit();
 if (((*ptr_to_globals).dot ) > ((*ptr_to_globals).text ) && ((*ptr_to_globals).dot )[-1] != '\n')
  ((*ptr_to_globals).dot )--;
}
static void dot_right(void)
{
 undo_queue_commit();
 if (((*ptr_to_globals).dot ) < ((*ptr_to_globals).end ) - 1 && *((*ptr_to_globals).dot ) != '\n')
  ((*ptr_to_globals).dot )++;
}
static void dot_begin(void)
{
 undo_queue_commit();
 ((*ptr_to_globals).dot ) = begin_line(((*ptr_to_globals).dot )); // return pointer to first char cur line
}
static void dot_end(void)
{
 undo_queue_commit();
 ((*ptr_to_globals).dot ) = end_line(((*ptr_to_globals).dot )); // return pointer to last char cur line
}
static char *move_to_col(char *p, int l)
{
 int co;
 p = begin_line(p);
 co = 0;
 do {
  if (*p == '\n') //vda || *p == '\0')
   break;
  co = next_column(*p, co);
 } while (co <= l && p++ < ((*ptr_to_globals).end ));
 return p;
}
static void dot_next(void)
{
 undo_queue_commit();
 ((*ptr_to_globals).dot ) = next_line(((*ptr_to_globals).dot ));
}
static void dot_prev(void)
{
 undo_queue_commit();
 ((*ptr_to_globals).dot ) = prev_line(((*ptr_to_globals).dot ));
}
static void dot_skip_over_ws(void)
{
 // skip WS
 while (((*ptr_to_globals).dot ) && ((*ptr_to_globals).dot ) < ((*ptr_to_globals).end ) - 1 && *((*ptr_to_globals).dot ) != '\n' && isspace((unsigned char)*((*ptr_to_globals).dot )))
  ((*ptr_to_globals).dot )++;
}
static void dot_to_char(int cmd)
{
 char *q = ((*ptr_to_globals).dot );
 int dir = ((*__ctype_b_loc ())[(int) ((cmd))] & (unsigned short int) _ISlower) ? FORWARD : BACK;
 if (((*ptr_to_globals).last_search_char ) == 0)
  return;
 do {
  do {
   q += dir;
   if ((dir == FORWARD ? q > ((*ptr_to_globals).end ) - 1 : q < ((*ptr_to_globals).text )) || *q == '\n') {
    indicate_error();
    return;
   }
  } while (*q != ((*ptr_to_globals).last_search_char ));
 } while (--((*ptr_to_globals).cmdcnt ) > 0);
 ((*ptr_to_globals).dot ) = q;
 // place cursor before/after char as required
 if (cmd == 't')
  dot_left();
 else if (cmd == 'T')
  dot_right();
}
static void dot_scroll(int cnt, int dir)
{
 char *q;
 undo_queue_commit();
 for (; cnt > 0; cnt--) {
  if (dir < 0) {
   // scroll Backwards
   // ctrl-Y scroll up one line
   ((*ptr_to_globals).screenbegin ) = prev_line(((*ptr_to_globals).screenbegin ));
  } else {
   // scroll Forwards
   // ctrl-E scroll down one line
   ((*ptr_to_globals).screenbegin ) = next_line(((*ptr_to_globals).screenbegin ));
  }
 }
 // make sure "dot" stays on the screen so we dont scroll off
 if (((*ptr_to_globals).dot ) < ((*ptr_to_globals).screenbegin ))
  ((*ptr_to_globals).dot ) = ((*ptr_to_globals).screenbegin );
 q = end_screen(); // find new bottom line
 if (((*ptr_to_globals).dot ) > q)
  ((*ptr_to_globals).dot ) = begin_line(q); // is dot is below bottom line?
 dot_skip_over_ws();
}
static char *bound_dot(char *p) // make sure  text[0] <= P < "end"
{
 if (p >= ((*ptr_to_globals).end ) && ((*ptr_to_globals).end ) > ((*ptr_to_globals).text )) {
  p = ((*ptr_to_globals).end ) - 1;
  indicate_error();
 }
 if (p < ((*ptr_to_globals).text )) {
  p = ((*ptr_to_globals).text );
  indicate_error();
 }
 return p;
}
static void start_new_cmd_q(char c)
{
 // get buffer for new cmd
 ((*ptr_to_globals).dotcnt ) = ((*ptr_to_globals).cmdcnt ) ?: 1;
 ((*ptr_to_globals).last_modifying_cmd )[0] = c;
 ((*ptr_to_globals).lmc_len ) = 1;
 ((*ptr_to_globals).adding2q ) = 1;
}
static void end_cmd_q(void)
{
 ((*ptr_to_globals).YDreg ) = 26; // go back to default Yank/Delete reg
 ((*ptr_to_globals).adding2q ) = 0;
}
// copy text into register, then delete text.
//
static char *yank_delete(char *start, char *stop, int buftype, int yf, int undo)
{
 char *p;
 // make sure start <= stop
 if (start > stop) {
  // they are backwards, reverse them
  p = start;
  start = stop;
  stop = p;
 }
 if (buftype == PARTIAL && *start == '\n')
  return start;
 p = start;
 text_yank(start, stop, ((*ptr_to_globals).YDreg ), buftype);
 if (yf == YANKDEL) {
  p = text_hole_delete(start, stop, undo);
 } // delete lines
 return p;
}
// might reallocate text[]!
static int file_insert(const char *fn, char *p, int initial)
{
 int cnt = -1;
 int fd, size;
 struct stat statbuf;
 if (p < ((*ptr_to_globals).text ))
  p = ((*ptr_to_globals).text );
 if (p > ((*ptr_to_globals).end ))
  p = ((*ptr_to_globals).end );
 fd = open(fn, 00);
 if (fd < 0) {
  if (!initial)
   status_line_bold_errno(fn);
  return cnt;
 }
 // Validate file
 if (fstat(fd, &statbuf) < 0) {
  status_line_bold_errno(fn);
  goto fi;
 }
 if (!((((statbuf.st_mode)) & 0170000) == (0100000))) {
  status_line_bold("'%s' is not a regular file", fn);
  goto fi;
 }
 size = (statbuf.st_size < 2147483647 ? (int)statbuf.st_size : 2147483647);
 p += text_hole_make(p, size);
 cnt = full_read(fd, p, size);
 if (cnt < 0) {
  status_line_bold_errno(fn);
  text_hole_delete(p, p + size - 1, 0); // un-do buffer insert
 } else if (cnt < size) {
  // There was a partial read, shrink unused space
  text_hole_delete(p + cnt, p + size - 1, 0);
  status_line_bold("can't read '%s'", fn);
 }
 else {
  undo_push_insert(p, size, 1);
 }
 fi:
 close(fd);
 if (initial
  && ((access(fn, 2) < 0) ||
  // root will always have access()
  // so we check fileperms too
  !(statbuf.st_mode & (0200 | (0200 >> 3) | ((0200 >> 3) >> 3)))
     )
 ) {
  ((((*ptr_to_globals).readonly_mode )) |= 0x01);
 }
 return cnt;
}
// find matching char of pair  ()  []  {}
// will crash if c is not one of these
static char *find_pair(char *p, const char c)
{
 const char *braces = "()[]{}";
 char match;
 int dir, level;
 dir = strchr(braces, c) - braces;
 dir ^= 1;
 match = braces[dir];
 dir = ((dir & 1) << 1) - 1; // 1 for ([{, -1 for )\}
 // look for match, count levels of pairs  (( ))
 level = 1;
 for (;;) {
  p += dir;
  if (p < ((*ptr_to_globals).text ) || p >= ((*ptr_to_globals).end ))
   return ((void*)0);
  if (*p == c)
   level++; // increase pair levels
  if (*p == match) {
   level--; // reduce pair level
   if (level == 0)
    return p; // found matching pair
  }
 }
}
// show the matching char of a pair,  ()  []  {}
static void showmatching(char *p)
{
 char *q, *save_dot;
 // we found half of a pair
 q = find_pair(p, *p); // get loc of matching char
 if (q == ((void*)0)) {
  indicate_error(); // no matching char
 } else {
  // "q" now points to matching pair
  save_dot = ((*ptr_to_globals).dot ); // remember where we are
  ((*ptr_to_globals).dot ) = q; // go to new loc
  refresh(0); // let the user see it
  mysleep(40); // give user some time
  ((*ptr_to_globals).dot ) = save_dot; // go back to old loc
  refresh(0);
 }
}
// might reallocate text[]! use p += stupid_insert(p, ...),
// and be careful to not use pointers into potentially freed text[]!
static uintptr_t stupid_insert(char *p, char c) // stupidly insert the char c at 'p'
{
 uintptr_t bias;
 bias = text_hole_make(p, 1);
 p += bias;
 *p = c;
 return bias;
}
// find number of characters in indent, p must be at beginning of line
static size_t indent_len(char *p)
{
 char *r = p;
 while (r < (((*ptr_to_globals).end ) - 1) && ((*__ctype_b_loc ())[(int) ((*r))] & (unsigned short int) _ISblank))
  r++;
 return r - p;
}
static char *char_insert(char *p, char c, int undo) // insert the char c at 'p'
{
 size_t len;
 int col, ntab, nspc;
 char *bol = begin_line(p);
 if (c == 22) { // Is this an ctrl-V?
  p += stupid_insert(p, '^'); // use ^ to indicate literal next
  refresh(0); // show the ^
  c = get_one_char();
  *p = c;
  undo_push_insert(p, 1, undo);
  p++;
 } else if (c == 27) { // Is this an ESC?
  ((*ptr_to_globals).cmd_mode ) = 0;
  undo_queue_commit();
  ((*ptr_to_globals).cmdcnt ) = 0;
  end_cmd_q(); // stop adding to q
  ((*ptr_to_globals).last_status_cksum ) = 0; // force status update
  if ((((*ptr_to_globals).dot ) > ((*ptr_to_globals).text )) && (p[-1] != '\n')) {
   p--;
  }
  if ((((*ptr_to_globals).vi_setops ) & (1 << 0))) {
   len = indent_len(bol);
   col = get_column(bol + len);
   if (len && col == ((*ptr_to_globals).char_insert__indentcol) && bol[len] == '\n') {
    // remove autoindent from otherwise empty line
    text_hole_delete(bol, bol + len - 1, undo);
    p = bol;
   }
  }
 } else if (c == 4) { // ctrl-D reduces indentation
  char *r = bol + indent_len(bol);
  int prev = prev_tabstop(get_column(r));
  while (r > bol && get_column(r) > prev) {
   if (p > bol)
    p--;
   r--;
   r = text_hole_delete(r, r, 3);
  }
  if ((((*ptr_to_globals).vi_setops ) & (1 << 0)) && ((*ptr_to_globals).char_insert__indentcol) && r == end_line(p)) {
   // record changed size of autoindent
   ((*ptr_to_globals).char_insert__indentcol) = get_column(p);
   return p;
  }
 } else if (c == '\t' && (((*ptr_to_globals).vi_setops ) & (1 << 1) )) { // expand tab
  col = get_column(p);
  col = next_tabstop(col) - col + 1;
  while (col--) {
   undo_push_insert(p, 1, undo);
   p += 1 + stupid_insert(p, ' ');
  }
 } else if (((c) == ((*ptr_to_globals).term_orig ).c_cc[2] || (c) == 8 || (c) == 127)) {
  if (((*ptr_to_globals).cmd_mode ) == 2) {
   // special treatment for backspace in Replace mode
   if (p > ((*ptr_to_globals).rstart )) {
    p--;
    undo_pop();
   }
  } else if (p > ((*ptr_to_globals).text )) {
   p--;
   p = text_hole_delete(p, p, 3); // shrink buffer 1 char
  }
 } else {
  // insert a char into text[]
  if (c == 13)
   c = '\n'; // translate \r to \n
  if (c == '\n')
   undo_queue_commit();
  undo_push_insert(p, 1, undo);
  p += 1 + stupid_insert(p, c); // insert the char
  if ((((*ptr_to_globals).vi_setops ) & (1 << 4) ) && strchr(")]}", c) != ((void*)0)) {
   showmatching(p - 1);
  }
  if ((((*ptr_to_globals).vi_setops ) & (1 << 0)) && c == '\n') { // auto indent the new line
   if (((*ptr_to_globals).newindent ) < 0) {
    // use indent of previous line
    bol = prev_line(p);
    len = indent_len(bol);
    col = get_column(bol + len);
    if (len && col == ((*ptr_to_globals).char_insert__indentcol)) {
     // previous line was empty except for autoindent
     // move the indent to the current line
     memmove(bol + 1, bol, len);
     *bol = '\n';
     return p;
    }
   } else {
    // for 'O'/'cc' commands add indent before newly inserted NL
    if (p != ((*ptr_to_globals).end ) - 1) // but not for 'cc' at EOF
     p--;
    col = ((*ptr_to_globals).newindent );
   }
   if (col) {
    // only record indent if in insert/replace mode or for
    // the 'o'/'O'/'cc' commands, which are switched to
    // insert mode early.
    ((*ptr_to_globals).char_insert__indentcol) = ((*ptr_to_globals).cmd_mode ) != 0 ? col : 0;
    if ((((*ptr_to_globals).vi_setops ) & (1 << 1) )) {
     ntab = 0;
     nspc = col;
    } else {
     ntab = col / ((*ptr_to_globals).tabstop );
     nspc = col % ((*ptr_to_globals).tabstop );
    }
    p += text_hole_make(p, ntab + nspc);
    undo_push_insert(p, ntab + nspc, undo);
    memset(p, '\t', ntab);
    p += ntab;
    memset(p, ' ', nspc);
    return p + nspc;
   }
  }
 }
 ((*ptr_to_globals).char_insert__indentcol) = 0;
 return p;
}
static void init_filename(char *fn)
{
 char *copy = xstrdup(fn);
 if (((*ptr_to_globals).current_filename ) == ((void*)0)) {
  ((*ptr_to_globals).current_filename ) = copy;
 } else {
  free(((*ptr_to_globals).alt_filename ));
  ((*ptr_to_globals).alt_filename ) = copy;
 }
}
static void update_filename(char *fn)
{
 if (fn == ((void*)0))
  return;
 if (((*ptr_to_globals).current_filename ) == ((void*)0) || strcmp(fn, ((*ptr_to_globals).current_filename )) != 0) {
  free(((*ptr_to_globals).alt_filename ));
  ((*ptr_to_globals).alt_filename ) = ((*ptr_to_globals).current_filename );
  ((*ptr_to_globals).current_filename ) = xstrdup(fn);
 }
}
// read text from file or create an empty buf
// will also update current_filename
static int init_text_buffer(char *fn)
{
 int rc;
 // allocate/reallocate text buffer
 free(((*ptr_to_globals).text ));
 ((*ptr_to_globals).text_size ) = 10240;
 ((*ptr_to_globals).screenbegin ) = ((*ptr_to_globals).dot ) = ((*ptr_to_globals).end ) = ((*ptr_to_globals).text ) = xzalloc(((*ptr_to_globals).text_size ));
 update_filename(fn);
 rc = file_insert(fn, ((*ptr_to_globals).text ), 1);
 if (rc <= 0 || *(((*ptr_to_globals).end ) - 1) != '\n') {
  // file doesn't exist or doesn't end in a newline.
  // insert a newline to the end
  char_insert(((*ptr_to_globals).end ), '\n', 0);
 }
 flush_undo_data();
 ((*ptr_to_globals).modified_count ) = 0;
 ((*ptr_to_globals).last_modified_count) = -1;
 // init the marks
 memset(((*ptr_to_globals).mark ), 0, sizeof(((*ptr_to_globals).mark )));
 return rc;
}
// might reallocate text[]! use p += string_insert(p, ...),
// and be careful to not use pointers into potentially freed text[]!
static uintptr_t string_insert(char *p, const char *s, int undo) // insert the string at 'p'
{
 uintptr_t bias;
 int i;
 i = strlen(s);
 undo_push_insert(p, i, undo);
 bias = text_hole_make(p, i);
 p += bias;
 memcpy(p, s, i);
 return bias;
}
static int file_write(char *fn, char *first, char *last)
{
 int fd, cnt, charcnt;
 if (fn == 0) {
  status_line_bold("No current filename");
  return -2;
 }
 // By popular request we do not open file with O_TRUNC,
 // but instead ftruncate() it _after_ successful write.
 // Might reduce amount of data lost on power fail etc.
 fd = open(fn, (01 | 0100), 0666);
 if (fd < 0)
  return -1;
 cnt = last - first + 1;
 charcnt = full_write(fd, first, cnt);
 ftruncate(fd, charcnt);
 if (charcnt == cnt) {
  // good write
  //modified_count = FALSE;
 } else {
  charcnt = 0;
 }
 close(fd);
 return charcnt;
}
// search for pattern starting at p
static char *char_search(char *p, const char *pat, int dir_and_range)
{
 struct re_pattern_buffer preg;
 const char *err;
 char *q;
 int i, size, range, start;
 re_syntax_options = ((((((unsigned long int) 1) << 1) << 1) | ((((((((unsigned long int) 1) << 1) << 1) << 1) << 1) << 1) << 1) | (((((((((unsigned long int) 1) << 1) << 1) << 1) << 1) << 1) << 1) << 1) | (((((((((((unsigned long int) 1) << 1) << 1) << 1) << 1) << 1) << 1) << 1) << 1) << 1) | ((((((((((((((((((unsigned long int) 1) << 1) << 1) << 1) << 1) << 1) << 1) << 1) << 1) << 1) << 1) << 1) << 1) << 1) << 1) << 1) << 1)) | (((unsigned long int) 1) << 1) | ((((((((((((((((((((((((((unsigned long int) 1) << 1) << 1) << 1) << 1) << 1) << 1) << 1) << 1) << 1) << 1) << 1) << 1) << 1) << 1) << 1) << 1) << 1) << 1) << 1) << 1) << 1) << 1) << 1) << 1)) & (~((((((((unsigned long int) 1) << 1) << 1) << 1) << 1) << 1) << 1));
 if ((((*ptr_to_globals).vi_setops ) & (1 << 3)))
  re_syntax_options |= ((((((((((((((((((((((((unsigned long int) 1) << 1) << 1) << 1) << 1) << 1) << 1) << 1) << 1) << 1) << 1) << 1) << 1) << 1) << 1) << 1) << 1) << 1) << 1) << 1) << 1) << 1) << 1);
 memset(&preg, 0, sizeof(preg));
 err = re_compile_pattern(pat, strlen(pat), &preg);
 preg.not_bol = p != ((*ptr_to_globals).text );
 preg.not_eol = p != ((*ptr_to_globals).end ) - 1;
 if (err != ((void*)0)) {
  status_line_bold("bad search pattern '%s': %s", pat, err);
  return p;
 }
 range = (dir_and_range & 1);
 q = ((*ptr_to_globals).end ) - 1; // if FULL
 if (range == LIMITED)
  q = next_line(p);
 if (dir_and_range < 0) { // BACK?
  q = ((*ptr_to_globals).text );
  if (range == LIMITED)
   q = prev_line(p);
 }
 // RANGE could be negative if we are searching backwards
 range = q - p;
 if (range < 0) {
  size = -range;
  start = size;
 } else {
  size = range;
  start = 0;
 }
 q = p - start;
 if (q < ((*ptr_to_globals).text ))
  q = ((*ptr_to_globals).text );
 // search for the compiled pattern, preg, in p[]
 // range < 0, start == size: search backward
 // range > 0, start == 0: search forward
 // re_search() < 0: not found or error
 // re_search() >= 0: index of found pattern
 //           struct pattern   char     int   int    int    struct reg
 // re_search(*pattern_buffer, *string, size, start, range, *regs)
 i = re_search(&preg, q, size, start, range, /*struct re_registers*:*/ ((void*)0));
 regfree(&preg);
 return i < 0 ? ((void*)0) : q + i;
}
//----- The Colon commands -------------------------------------
// Evaluate colon address expression.  Returns a pointer to the
// next character or NULL on error.  If 'result' contains a valid
// address 'valid' is TRUE.
static char *get_one_address(char *p, int *result, int *valid)
{
 int num, sign, addr, got_addr;
 char *q, c;
 int dir;
 got_addr = 0;
 addr = count_lines(((*ptr_to_globals).text ), ((*ptr_to_globals).dot )); // default to current line
 sign = 0;
 for (;;) {
  if (((*__ctype_b_loc ())[(int) ((*p))] & (unsigned short int) _ISblank)) {
   if (got_addr) {
    addr += sign;
    sign = 0;
   }
   p++;
  } else if (!got_addr && *p == '.') { // the current line
   p++;
   //addr = count_lines(text, dot);
   got_addr = 1;
  } else if (!got_addr && *p == '$') { // the last line in file
   p++;
   addr = count_lines(((*ptr_to_globals).text ), ((*ptr_to_globals).end ) - 1);
   got_addr = 1;
  }
  else if (!got_addr && *p == '\'') { // is this a mark addr
   p++;
   c = tolower(*p);
   p++;
   q = ((void*)0);
   if (c >= 'a' && c <= 'z') {
    // we have a mark
    c = c - 'a';
    q = ((*ptr_to_globals).mark )[(unsigned char) c];
   }
   if (q == ((void*)0)) { // is mark valid
    status_line_bold("Mark not set");
    return ((void*)0);
   }
   addr = count_lines(((*ptr_to_globals).text ), q);
   got_addr = 1;
  }
  else if (!got_addr && (*p == '/' || *p == '?')) { // a search pattern
   c = *p;
   q = strchrnul(p + 1, c);
   if (p + 1 != q) {
    // save copy of new pattern
    free(((*ptr_to_globals).last_search_pattern));
    ((*ptr_to_globals).last_search_pattern) = xstrndup(p, q - p);
   }
   p = q;
   if (*p == c)
    p++;
   if (c == '/') {
    q = next_line(((*ptr_to_globals).dot ));
    dir = (FORWARD << 1) | FULL;
   } else {
    q = begin_line(((*ptr_to_globals).dot ));
    dir = ((unsigned)BACK << 1) | FULL;
   }
   q = char_search(q, ((*ptr_to_globals).last_search_pattern) + 1, dir);
   if (q == ((void*)0)) {
    // no match, continue from other end of file
    q = char_search(dir > 0 ? ((*ptr_to_globals).text ) : ((*ptr_to_globals).end ) - 1,
        ((*ptr_to_globals).last_search_pattern) + 1, dir);
    if (q == ((void*)0)) {
     status_line_bold("Pattern not found");
     return ((void*)0);
    }
   }
   addr = count_lines(((*ptr_to_globals).text ), q);
   got_addr = 1;
  }
  else if (((*__ctype_b_loc ())[(int) ((*p))] & (unsigned short int) _ISdigit)) {
   num = 0;
   while (((*__ctype_b_loc ())[(int) ((*p))] & (unsigned short int) _ISdigit))
    num = num * 10 + *p++ -'0';
   if (!got_addr) { // specific line number
    addr = num;
    got_addr = 1;
   } else { // offset from current addr
    addr += sign >= 0 ? num : -num;
   }
   sign = 0;
  } else if (*p == '-' || *p == '+') {
   if (!got_addr) { // default address is dot
    //addr = count_lines(text, dot);
    got_addr = 1;
   } else {
    addr += sign;
   }
   sign = *p++ == '-' ? -1 : 1;
  } else {
   addr += sign; // consume unused trailing sign
   break;
  }
 }
 *result = addr;
 *valid = got_addr;
 return p;
}
// Read line addresses for a colon command.  The user can enter as
// many as they like but only the last two will be used.
static char *get_address(char *p, int *b, int *e, unsigned *got)
{
 int state = 0;
 int valid;
 int addr;
 char *save_dot = ((*ptr_to_globals).dot );
 //----- get the address' i.e., 1,3   'a,'b  -----
 for (;;) {
  if (((*__ctype_b_loc ())[(int) ((*p))] & (unsigned short int) _ISblank)) {
   p++;
  } else if (state == 0 && *p == '%') { // alias for 1,$
   p++;
   *b = 1;
   *e = count_lines(((*ptr_to_globals).text ), ((*ptr_to_globals).end )-1);
   *got = 3;
   state = 1;
  } else if (state == 0) {
   valid = 0;
   p = get_one_address(p, &addr, &valid);
   // Quit on error or if the address is invalid and isn't of
   // the form ',$' or '1,' (in which case it defaults to dot).
   if (p == ((void*)0) || !(valid || *p == ',' || *p == ';' || *got & 1))
    break;
   *b = *e;
   *e = addr;
   *got = (*got << 1) | 1;
   state = 1;
  } else if (state == 1 && (*p == ',' || *p == ';')) {
   if (*p == ';')
    ((*ptr_to_globals).dot ) = find_line(*e);
   p++;
   state = 0;
  } else {
   break;
  }
 }
 ((*ptr_to_globals).dot ) = save_dot;
 return p;
}
static void setops(char *args, int flg_no)
{
 char *eq;
 int index;
 eq = strchr(args, '=');
 if (eq) *eq = '\0';
 index = index_in_strings("ai\0""autoindent\0" "et\0""expandtab\0" "fl\0""flash\0" "ic\0""ignorecase\0" "sm\0""showmatch\0" "ts\0""tabstop\0", args + flg_no);
 if (eq) *eq = '=';
 if (index < 0) {
 bad:
  status_line_bold("bad option: %s", args);
  return;
 }
 index = 1 << (index >> 1); // convert to VI_bit
 if (index & (1 << 5)) {
  int t;
  if (!eq || flg_no) // no "=NNN" or it is "notabstop"?
   goto bad;
  t = vi_strtou(eq + 1, ((void*)0), 10);
  if (t <= 0 || t > MAX_TABSTOP)
   goto bad;
  ((*ptr_to_globals).tabstop ) = t;
  return;
 }
 if (eq) goto bad; // boolean option has "="?
 if (flg_no) {
  ((*ptr_to_globals).vi_setops ) &= ~index;
 } else {
  ((*ptr_to_globals).vi_setops ) |= index;
 }
}
static char *expand_args(char *args)
{
 char *s;
 const char *replace;
 args = xstrdup(args);
 for (s = args; *s; s++) {
  unsigned n;
  if (*s == '%') {
   replace = ((*ptr_to_globals).current_filename );
  } else if (*s == '#') {
   replace = ((*ptr_to_globals).alt_filename );
  } else {
   if (*s == '\\' && s[1] != '\0') {
    char *t;
    for (t = s; *t; t++)
     *t = t[1];
    s++;
   }
   continue;
  }
  if (replace == ((void*)0)) {
   free(args);
   status_line_bold("No previous filename");
   return ((void*)0);
  }
  n = (s - args);
  ((args) = xasprintf_and_free((args), "%.*s%s%s", n, args, replace, s+1));
  s = args + n + strlen(replace);
 }
 return args;
}
// Like strchr() but skipping backslash-escaped characters
static char *strchr_backslash(const char *s, int c)
{
 while (*s) {
  if (*s == c)
   return (char *)s;
  if (*s == '\\')
   if (*++s == '\0')
    break;
  s++;
 }
 return ((void*)0);
}
// If the return value is not NULL the caller should free R
static char *regex_search(char *q, regex_t *preg, const char *Rorig,
    size_t *len_F, size_t *len_R, char **R)
{
 regmatch_t regmatch[10], *cur_match;
 char *found = ((void*)0);
 const char *t;
 char *r;
 regmatch[0].rm_so = 0;
 regmatch[0].rm_eo = end_line(q) - q;
 if (regexec(preg, q, 10, regmatch, (1 << 2)) != 0)
  return found;
 found = q + regmatch[0].rm_so;
 *len_F = regmatch[0].rm_eo - regmatch[0].rm_so;
 *R = ((void*)0);
 fill_result:
 // first pass calculates len_R, second fills R
 *len_R = 0;
 for (t = Rorig, r = *R; *t; t++) {
  size_t len = 1; // default is to copy one char from replace pattern
  const char *from = t;
  if (*t == '\\') {
   from = ++t; // skip backslash
   if (*t >= '0' && *t < '0' + 10) {
    cur_match = regmatch + (*t - '0');
    if (cur_match->rm_so >= 0) {
     len = cur_match->rm_eo - cur_match->rm_so;
     from = q + cur_match->rm_so;
    }
   }
  }
  *len_R += len;
  if (*R) {
   memcpy(r, from, len);
   r += len;
   /* *r = '\0'; - xzalloc did it */
  }
 }
 if (*R == ((void*)0)) {
  *R = xzalloc(*len_R + 1);
  goto fill_result;
 }
 return found;
}
static void colon(char *buf)
{
 char cmd[sizeof("features!")]; // longest known command + NUL
 char *args;
 int cmdlen;
 char *useforce;
 char *q, *r;
 int b, e;
// check how many addresses we got
 unsigned got;
 char *exp = ((void*)0); // may hold expand_args() result: if VI_COLON_EXPAND, needs freeing!
 // :3154	// if (-e line 3154) goto it  else stay put
 // :4,33w! foo	// write a portion of buffer to file "foo"
 // :w		// write all of buffer to current file
 // :q		// quit
 // :q!		// quit- dont care about modified file
 // :'a,'z!sort -u   // filter block through sort
 // :'f		// goto mark "f"
 // :'fl		// list literal the mark "f" line
 // :.r bar	// read file "bar" into buffer before dot
 // :/123/,/abc/d    // delete lines from "123" line to "abc" line
 // :/xyz/	// goto the "xyz" line
 // :s/find/replace/ // substitute pattern "find" with "replace"
 // :!<cmd>	// run <cmd> then return
 while (*buf == ':')
  buf++; // move past leading colons
 buf = skip_whitespace(buf); // move past leading blanks
 if (!*buf || *buf == '"')
  goto ret; // ignore empty lines or those starting with '"'
 // look for optional address(es)  ":." ":1" ":1,9" ":'q,'a" ":%"
 b = e = -1;
 got = 0;
 buf = get_address(buf, &b, &e, &got);
 if (buf == ((void*)0))
  goto ret;
 // get the COMMAND into cmd[]
 safe_strncpy(cmd, buf, sizeof(cmd));
 skip_non_whitespace(cmd)[0] = '\0';
 useforce = last_char_is(cmd, '!');
 if (useforce && useforce > cmd)
  *useforce = '\0'; // "CMD!" -> "CMD" (unless single "!")
 // find ARGuments
 args = skip_whitespace(skip_non_whitespace(buf));
 // assume the command will want a range, certain commands
 // (read, substitute) need to adjust these assumptions
 q = ((*ptr_to_globals).text ); // if no addr, use 1,$ for the range
 r = ((*ptr_to_globals).end ) - 1;
 if ((got & 1)) { // at least one addr was given, get its details
  int lines;
  if (e < 0
   || e > (lines = count_lines(((*ptr_to_globals).text ), ((*ptr_to_globals).end ) - 1))
  ) {
   status_line_bold("Invalid range");
   goto ret;
  }
  q = r = find_line(e);
  if (!((got & 3) == 3)) {
   // if there is only one addr, then it's the line
   // number of the single line the user wants.
   // Reset the end pointer to the end of that line.
   r = end_line(q);
  } else {
   // we were given two addrs.  change the
   // start pointer to the addr given by user.
   if (b < 0 || b > lines || b > e) {
    status_line_bold("Invalid range");
    goto ret;
   }
   q = find_line(b); // what line is #b
   r = end_line(r);
  }
 }
 // ------------ now look for the command ------------
 cmdlen = strlen(cmd);
 if (cmdlen == 0) { // ":123<enter>" - goto line #123
  if (e >= 0) {
   ((*ptr_to_globals).dot ) = find_line(e); // what line is #e
   dot_skip_over_ws();
  }
 }
 else if (cmd[0] == '=' && !cmd[1]) { // where is the address
  if (!(got & 1)) { // no addr given- use defaults
   e = count_lines(((*ptr_to_globals).text ), ((*ptr_to_globals).dot ));
  }
  status_line("%d", e);
 } else if (strncmp(cmd, "delete", cmdlen) == 0) { // delete lines
  if (!(got & 1)) { // no addr given- use defaults
   q = begin_line(((*ptr_to_globals).dot )); // assume .,. for the range
   r = end_line(((*ptr_to_globals).dot ));
  }
  ((*ptr_to_globals).dot ) = yank_delete(q, r, WHOLE, YANKDEL, 1); // save, then delete lines
  dot_skip_over_ws();
 } else if (strncmp(cmd, "edit", cmdlen) == 0) { // Edit a file
  int size;
  char *fn;
  // don't edit, if the current file has been modified
  if (((*ptr_to_globals).modified_count ) && !useforce) {
   status_line_bold("No write since last change (:%s! overrides)", cmd);
   goto ret;
  }
  fn = ((*ptr_to_globals).current_filename );
  if (args[0]) {
   // the user supplied a file name
   fn = expand_args(args);
   if (fn == ((void*)0))
    goto ret;
  } else if (((*ptr_to_globals).current_filename ) == ((void*)0)) {
   // no user file name, no current name- punt
   status_line_bold("No current filename");
   goto ret;
  }
  size = init_text_buffer(fn);
  if (27 >= 0 && 27 < 28) {
   free(((*ptr_to_globals).reg )[27]); //   free orig line reg- for 'U'
   ((*ptr_to_globals).reg )[27] = ((void*)0);
  }
  /*if (YDreg < 28) - always true*/ {
   free(((*ptr_to_globals).reg )[((*ptr_to_globals).YDreg )]); //   free default yank/delete register
   ((*ptr_to_globals).reg )[((*ptr_to_globals).YDreg )] = ((void*)0);
  }
  status_line("'%s'%s"
   "%s"
   " %uL, %uC",
   fn,
   (size < 0 ? " [New file]" : ""),
   ((((*ptr_to_globals).readonly_mode )) ? " [Readonly]" : ""),
   count_lines(((*ptr_to_globals).text ), ((*ptr_to_globals).end ) - 1),
   (int)(((*ptr_to_globals).end ) - ((*ptr_to_globals).text ))
  );
 } else if (strncmp(cmd, "file", cmdlen) == 0) { // what File is this
  if (e >= 0) {
   status_line_bold("No address allowed on this command");
   goto ret;
  }
  if (args[0]) {
   // user wants a new filename
   exp = expand_args(args);
   if (exp == ((void*)0))
    goto ret;
   update_filename(exp);
  } else {
   // user wants file status info
   ((*ptr_to_globals).last_status_cksum ) = 0; // force status update
  }
 } else if (strncmp(cmd, "features", cmdlen) == 0) { // what features are available
  // print out values of all features
  go_bottom_and_clear_to_eol();
  cookmode();
  show_help();
  rawmode();
  Hit_Return();
 } else if (strncmp(cmd, "list", cmdlen) == 0) { // literal print line
  char *dst;
  if (!(got & 1)) { // no addr given- use defaults
   q = begin_line(((*ptr_to_globals).dot )); // assume .,. for the range
   r = end_line(((*ptr_to_globals).dot ));
  }
  ((*ptr_to_globals).have_status_msg ) = 1;
  dst = ((*ptr_to_globals).status_buffer );
  while (q <= r && dst < ((*ptr_to_globals).status_buffer ) + 200 - (sizeof("\033""[7m" "^?" "\033""[m") + 1)) {
   char c;
   int c_is_no_print;
   c = *q++;
   if (c == '\n') {
    *dst++ = '$';
    break;
   }
   c_is_no_print = (c & 0x80) && !((unsigned char)(c) >= ' ' && (c) != 0x7f && (unsigned char)(c) != 0x9b);
   if (c_is_no_print) {
//TODO: print fewer ESC if more than one ctrl char
    dst = stpcpy(dst, "\033""[7m");
    *dst++ = '.';
    dst = stpcpy(dst, "\033""[m");
    continue;
   }
   if (c < ' ' || c == 127) {
    *dst++ = '^';
    if (c == 127)
     c = '?';
    else
     c += '@';
   }
   *dst++ = c;
  }
  *dst = '\0';
 } else if (strncmp(cmd, "quit", cmdlen) == 0 // quit
         || strncmp(cmd, "next", cmdlen) == 0 // edit next file
         || strncmp(cmd, "prev", cmdlen) == 0 // edit previous file
 ) {
  int n;
  if (useforce) {
   if (*cmd == 'q') {
    // force end of argv list
    optind = ((*ptr_to_globals).cmdline_filecnt );
   }
   ((*ptr_to_globals).editing ) = 0;
   goto ret;
  }
  // don't exit if the file been modified
  if (((*ptr_to_globals).modified_count )) {
   status_line_bold("No write since last change (:%s! overrides)", cmd);
   goto ret;
  }
  // are there other file to edit
  n = ((*ptr_to_globals).cmdline_filecnt ) - optind - 1;
  if (*cmd == 'q' && n > 0) {
   status_line_bold("%u more file(s) to edit", n);
   goto ret;
  }
  if (*cmd == 'n' && n <= 0) {
   status_line_bold("No more files to edit");
   goto ret;
  }
  if (*cmd == 'p') {
   // are there previous files to edit
   if (optind < 1) {
    status_line_bold("No previous files to edit");
    goto ret;
   }
   optind -= 2;
  }
  ((*ptr_to_globals).editing ) = 0;
 } else if (strncmp(cmd, "read", cmdlen) == 0) { // read file into text[]
  int size, num;
  char *fn = ((*ptr_to_globals).current_filename );
  if (args[0]) {
   // the user supplied a file name
   fn = expand_args(args);
   if (fn == ((void*)0))
    goto ret;
   init_filename(fn);
  } else if (((*ptr_to_globals).current_filename ) == ((void*)0)) {
   // no user file name, no current name- punt
   status_line_bold("No current filename");
   goto ret;
  }
  if (e == 0) { // user said ":0r foo"
   q = ((*ptr_to_globals).text );
  } else { // read after given line or current line if none given
   q = next_line((got & 1) ? find_line(e) : ((*ptr_to_globals).dot ));
   // read after last line
   if (q == ((*ptr_to_globals).end )-1)
    ++q;
  }
  num = count_lines(((*ptr_to_globals).text ), q);
  if (q == ((*ptr_to_globals).end ))
   num++;
  { // dance around potentially-reallocated text[]
   uintptr_t ofs = q - ((*ptr_to_globals).text );
   size = file_insert(fn, q, 0);
   q = ((*ptr_to_globals).text ) + ofs;
  }
  if (size < 0)
   goto ret; // nothing was inserted
  status_line("'%s'"
   "%s"
   " %uL, %uC",
   fn,
   (((*ptr_to_globals).readonly_mode ) ? " [Readonly]" : ""),
   count_lines(q, q + size - 1),
   size
  );
  ((*ptr_to_globals).dot ) = find_line(num);
 } else if (strncmp(cmd, "rewind", cmdlen) == 0) { // rewind cmd line args
  if (((*ptr_to_globals).modified_count ) && !useforce) {
   status_line_bold("No write since last change (:%s! overrides)", cmd);
  } else {
   // reset the filenames to edit
   optind = -1; // start from 0th file
   ((*ptr_to_globals).editing ) = 0;
  }
 } else if (strncmp(cmd, "set", cmdlen) == 0 // set or clear features
  && cmdlen > 1 // (do not confuse with "s /find/repl/")
 ) {
  char *argp, *argn, oldch;
  if (!args[0] || strcmp(args, "all") == 0) {
   // print out values of all options
   status_line_bold(
    "%sautoindent "
    "%sexpandtab "
    "%sflash "
    "%signorecase "
    "%sshowmatch "
    "tabstop=%u",
    (((*ptr_to_globals).vi_setops ) & (1 << 0)) ? "" : "no",
    (((*ptr_to_globals).vi_setops ) & (1 << 1) ) ? "" : "no",
    (((*ptr_to_globals).vi_setops ) & (1 << 2)) ? "" : "no",
    (((*ptr_to_globals).vi_setops ) & (1 << 3)) ? "" : "no",
    (((*ptr_to_globals).vi_setops ) & (1 << 4) ) ? "" : "no",
    ((*ptr_to_globals).tabstop )
   );
   goto ret;
  }
  argp = args;
  while (*argp) {
   int i = 0;
   if (argp[0] == 'n' && argp[1] == 'o') // "noXXX"
    i = 2;
   argn = skip_non_whitespace(argp);
   oldch = *argn;
   *argn = '\0';
   setops(argp, i);
   *argn = oldch;
   argp = skip_whitespace(argn);
  }
 } else if (cmd[0] == 's') { // substitute a pattern with a replacement pattern
  char c;
  char *F, *R, *flags;
  size_t len_F, len_R;
  int i;
  int gflag = 0; // global replace flag
  int subs = 0; // number of substitutions
  int last_line = 0, lines = 0;
  regex_t preg;
  int cflags;
  char *Rorig;
  int undo = 0;
  buf = skip_whitespace(buf + 1); // spaces allowed: "s  /find/repl/"
  // F points to the "find" pattern
  // R points to the "replace" pattern
  // replace the cmd line delimiters "/" with NULs
  c = buf[0]; // what is the delimiter
  F = buf + 1; // start of "find"
  R = strchr_backslash(F, c); // middle delimiter
  if (!R)
   goto colon_s_fail;
  len_F = R - F;
  *R++ = '\0'; // terminate "find"
  flags = strchr_backslash(R, c);
  if (flags) {
   *flags++ = '\0'; // terminate "replace"
   gflag = *flags;
  }
  if (len_F) { // save "find" as last search pattern
   free(((*ptr_to_globals).last_search_pattern));
   ((*ptr_to_globals).last_search_pattern) = xstrdup(F - 1);
   ((*ptr_to_globals).last_search_pattern)[0] = '/';
  } else if (((*ptr_to_globals).last_search_pattern)[1] == '\0') {
   status_line_bold("No previous search");
   goto ret;
  } else {
   F = ((*ptr_to_globals).last_search_pattern) + 1;
   len_F = strlen(F);
  }
  if (!(got & 1)) { // no addr given
   q = begin_line(((*ptr_to_globals).dot )); // start with cur line
   b = e = count_lines(((*ptr_to_globals).text ), q); // cur line number
  } else if (!((got & 3) == 3)) { // one addr given
   b = e;
  }
  Rorig = R;
  cflags = 0;
  if ((((*ptr_to_globals).vi_setops ) & (1 << 3)))
   cflags = (1 << 1);
  memset(&preg, 0, sizeof(preg));
  if (regcomp(&preg, F, cflags) != 0) {
   status_line(":s bad search pattern");
   goto regex_search_end;
  }
  for (i = b; i <= e; i++) { // so, :20,23 s \0 find \0 replace \0
   char *ls = q; // orig line start
   char *found;
 vc4:
   found = regex_search(q, &preg, Rorig, &len_F, &len_R, &R);
   if (found) {
    uintptr_t bias;
    // we found the "find" pattern - delete it
    // For undo support, the first item should not be chained
    // This needs to be handled differently depending on
    // whether or not regex support is enabled.
    if (len_F) // match can be empty, no delete needed
     text_hole_delete(found, found + len_F - 1,
        undo++ ? 2 : 1);
    if (len_R != 0) { // insert the "replace" pattern, if required
     bias = string_insert(found, R,
        undo++ ? 2 : 1);
     found += bias;
     ls += bias;
     //q += bias; - recalculated anyway
    }
    free(R);
    if (len_F || len_R != 0) {
     ((*ptr_to_globals).dot ) = ls;
     subs++;
     if (last_line != i) {
      last_line = i;
      ++lines;
     }
    }
    // check for "global"  :s/foo/bar/g
    if (gflag == 'g') {
     if ((found + len_R) < end_line(ls)) {
      q = found + len_R;
      goto vc4; // don't let q move past cur line
     }
    }
   }
   q = next_line(ls);
  }
  if (subs == 0) {
   status_line_bold("No match");
  } else {
   dot_skip_over_ws();
   if (subs > 1)
    status_line("%d substitutions on %d lines", subs, lines);
  }
 regex_search_end:
  regfree(&preg);
 } else if (strncmp(cmd, "version", cmdlen) == 0) { // show software version
  status_line("standalone");
 } else if (strncmp(cmd, "write", cmdlen) == 0 // write text to file
         || strcmp(cmd, "wq") == 0
         || strcmp(cmd, "wn") == 0
         || (cmd[0] == 'x' && !cmd[1])
 ) {
  int size, l;
  //int forced = FALSE;
  char *fn = ((*ptr_to_globals).current_filename );
  // is there a file name to write to?
  if (args[0]) {
   struct stat statbuf;
   exp = expand_args(args);
   if (exp == ((void*)0))
    goto ret;
   if (!useforce
    && (fn == ((void*)0) || strcmp(fn, exp) != 0)
    && stat(exp, &statbuf) == 0
   ) {
    status_line_bold("File exists (:w! overrides)");
    goto ret;
   }
   fn = exp;
   init_filename(fn);
  }
  else if (((*ptr_to_globals).readonly_mode ) && !useforce && fn) {
   status_line_bold("'%s' is read only", fn);
   goto ret;
  }
  //if (useforce) {
   // if "fn" is not write-able, chmod u+w
   // sprintf(syscmd, "chmod u+w %s", fn);
   // system(syscmd);
   // forced = TRUE;
  //}
  size = l = 0;
  if (((*ptr_to_globals).modified_count ) != 0 || cmd[0] != 'x') {
   size = r - q + 1;
   l = file_write(fn, q, r);
  }
  //if (useforce && forced) {
   // chmod u-w
   // sprintf(syscmd, "chmod u-w %s", fn);
   // system(syscmd);
   // forced = FALSE;
  //}
  if (l < 0) {
   if (l == -1)
    status_line_bold_errno(fn);
  } else {
   // how many lines written
   int lines = count_lines(q, q + l - 1);
   status_line("'%s' %uL, %uC", fn, lines, l);
   if (l == size) {
    if (q == ((*ptr_to_globals).text ) && q + l == ((*ptr_to_globals).end )) {
     ((*ptr_to_globals).modified_count ) = 0;
     ((*ptr_to_globals).last_modified_count) = -1;
    }
    if (cmd[1] == 'n') {
     ((*ptr_to_globals).editing ) = 0;
    } else if (cmd[0] == 'x' || cmd[1] == 'q') {
     // are there other files to edit?
     int n = ((*ptr_to_globals).cmdline_filecnt ) - optind - 1;
     if (n > 0) {
      if (!useforce) {
       status_line_bold("%u more file(s) to edit", n);
       goto ret;
      }
      // force end of argv list
      optind = ((*ptr_to_globals).cmdline_filecnt );
     }
     ((*ptr_to_globals).editing ) = 0;
    }
   }
  }
 } else if (strncmp(cmd, "yank", cmdlen) == 0) { // yank lines
  int lines;
  if (!(got & 1)) { // no addr given- use defaults
   q = begin_line(((*ptr_to_globals).dot )); // assume .,. for the range
   r = end_line(((*ptr_to_globals).dot ));
  }
  text_yank(q, r, ((*ptr_to_globals).YDreg ), WHOLE);
  lines = count_lines(q, r);
  status_line("Yank %d lines (%d chars) into [%c]",
    lines, strlen(((*ptr_to_globals).reg )[((*ptr_to_globals).YDreg )]), what_reg());
 } else {
  // cmd unknown
  not_implemented(cmd);
 }
 ret:
 free(exp);
 ((*ptr_to_globals).dot ) = bound_dot(((*ptr_to_globals).dot )); // make sure "dot" is valid
 return;
 colon_s_fail:
 status_line(":s expression missing delimiters");
}
//----- Char Routines --------------------------------------------
// Chars that are part of a word-
//    0123456789_ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz
// Chars that are Not part of a word (stoppers)
//    !"#$%&'()*+,-./:;<=>?@[\]^`{|}~
// Chars that are WhiteSpace
//    TAB NEWLINE VT FF RETURN SPACE
// DO NOT COUNT NEWLINE AS WHITESPACE
static int st_test(char *p, int type, int dir, char *tested)
{
 char c, c0, ci;
 int test, inc;
 inc = dir;
 c = c0 = p[0];
 ci = p[inc];
 test = 0;
 if (type == S_BEFORE_WS) {
  c = ci;
  test = (!((*__ctype_b_loc ())[(int) ((c))] & (unsigned short int) _ISspace) || c == '\n');
 }
 if (type == S_TO_WS) {
  c = c0;
  test = (!((*__ctype_b_loc ())[(int) ((c))] & (unsigned short int) _ISspace) || c == '\n');
 }
 if (type == S_OVER_WS) {
  c = c0;
  test = ((*__ctype_b_loc ())[(int) ((c))] & (unsigned short int) _ISspace);
 }
 if (type == S_END_PUNCT) {
  c = ci;
  test = ((*__ctype_b_loc ())[(int) ((c))] & (unsigned short int) _ISpunct);
 }
 if (type == S_END_ALNUM) {
  c = ci;
  test = (((*__ctype_b_loc ())[(int) ((c))] & (unsigned short int) _ISalnum) || c == '_');
 }
 *tested = c;
 return test;
}
static char *skip_thing(char *p, int linecnt, int dir, int type)
{
 char c;
 while (st_test(p, type, dir, &c)) {
  // make sure we limit search to correct number of lines
  if (c == '\n' && --linecnt < 1)
   break;
  if (dir >= 0 && p >= ((*ptr_to_globals).end ) - 1)
   break;
  if (dir < 0 && p <= ((*ptr_to_globals).text ))
   break;
  p += dir; // move to next char
 }
 return p;
}
static void winch_handler(int sig __attribute__((unused)))
{
 int save_errno = (*__errno_location ());
 // FIXME: do it in main loop!!!
 signal(28, winch_handler);
 query_screen_dimensions();
 new_screen(((*ptr_to_globals).rows ), ((*ptr_to_globals).columns )); // get memory for virtual screen
 redraw(1); // re-draw the screen
 (*__errno_location ()) = save_errno;
}
static void tstp_handler(int sig __attribute__((unused)))
{
 int save_errno = (*__errno_location ());
 // ioctl inside cookmode() was seen to generate SIGTTOU,
 // stopping us too early. Prevent that:
 signal(22, ((__sighandler_t) 1));
 go_bottom_and_clear_to_eol();
 cookmode(); // terminal to "cooked"
 // stop now
 //signal(SIGTSTP, SIG_DFL);
 //raise(SIGTSTP);
 raise(19); // avoid "dance" with TSTP handler - use SIGSTOP instead
 //signal(SIGTSTP, tstp_handler);
 // we have been "continued" with SIGCONT, restore screen and termios
 rawmode(); // terminal to "raw"
 ((*ptr_to_globals).last_status_cksum ) = 0; // force status update
 redraw(1); // re-draw the screen
 (*__errno_location ()) = save_errno;
}
static void int_handler(int sig)
{
 signal(2, int_handler);
 siglongjmp(((*ptr_to_globals).restart ), sig);
}
static void do_cmd(int c);
static int at_eof(const char *s)
{
 // does 's' point to end of file, even with no terminating newline?
 return ((s == ((*ptr_to_globals).end ) - 2 && s[1] == '\n') || s == ((*ptr_to_globals).end ) - 1);
}
static int find_range(char **start, char **stop, int cmd)
{
 char *p, *q, *t;
 int buftype = -1;
 int c;
 p = ((*ptr_to_globals).dot );
 if (cmd == 'Y') {
  c = 'y';
 } else
 {
  c = get_motion_char();
 }
 if ((cmd == 'Y' || cmd == c) && strchr("cdy><", c)) {
  // these cmds operate on whole lines
  buftype = WHOLE;
  if (--((*ptr_to_globals).cmdcnt ) > 0) {
   do_cmd('j');
   if (((*ptr_to_globals).cmd_error ))
    buftype = -1;
  }
 } else if (strchr("^%$0bBeEfFtThnN/?|{}\b\177", c)) {
  // Most operate on char positions within a line.  Of those that
  // don't '%' needs no special treatment, search commands are
  // marked as MULTI and  "{}" are handled below.
  buftype = strchr("nN/?", c) ? MULTI : PARTIAL;
  do_cmd(c); // execute movement cmd
  if (p == ((*ptr_to_globals).dot )) // no movement is an error
   buftype = -1;
 } else if (strchr("wW", c)) {
  buftype = MULTI;
  do_cmd(c); // execute movement cmd
  // step back one char, but not if we're at end of file,
  // or if we are at EOF and search was for 'w' and we're at
  // the start of a 'W' word.
  if (((*ptr_to_globals).dot ) > p && (!at_eof(((*ptr_to_globals).dot )) || (c == 'w' && ((*__ctype_b_loc ())[(int) ((*((*ptr_to_globals).dot )))] & (unsigned short int) _ISpunct))))
   ((*ptr_to_globals).dot )--;
  t = ((*ptr_to_globals).dot );
  // don't include trailing WS as part of word
  while (((*ptr_to_globals).dot ) > p && ((*__ctype_b_loc ())[(int) ((*((*ptr_to_globals).dot )))] & (unsigned short int) _ISspace)) {
   if (*((*ptr_to_globals).dot )-- == '\n')
    t = ((*ptr_to_globals).dot );
  }
  // for non-change operations WS after NL is not part of word
  if (cmd != 'c' && ((*ptr_to_globals).dot ) != t && *((*ptr_to_globals).dot ) != '\n')
   ((*ptr_to_globals).dot ) = t;
 } else if (strchr("GHL+-gjk'\r\n", c)) {
  // these operate on whole lines
  buftype = WHOLE;
  do_cmd(c); // execute movement cmd
  if (((*ptr_to_globals).cmd_error ))
   buftype = -1;
 } else if (c == ' ' || c == 'l') {
  // forward motion by character
  int tmpcnt = (((*ptr_to_globals).cmdcnt ) ?: 1);
  buftype = PARTIAL;
  do_cmd(c); // execute movement cmd
  // exclude last char unless range isn't what we expected
  // this indicates we've hit EOL
  if (tmpcnt == ((*ptr_to_globals).dot ) - p)
   ((*ptr_to_globals).dot )--;
 }
 if (buftype == -1) {
  if (c != 27)
   indicate_error();
  return buftype;
 }
 q = ((*ptr_to_globals).dot );
 if (q < p) {
  t = q;
  q = p;
  p = t;
 }
 // movements which don't include end of range
 if (q > p) {
  if (strchr("^0bBFThnN/?|\b\177", c)) {
   q--;
  } else if (strchr("{}", c)) {
   buftype = (p == begin_line(p) && (*q == '\n' || at_eof(q))) ?
       WHOLE : MULTI;
   if (!at_eof(q)) {
    q--;
    if (q > p && p != begin_line(p))
     q--;
   }
  }
 }
 *start = p;
 *stop = q;
 return buftype;
}
//---------------------------------------------------------------------
//----- the Ascii Chart -----------------------------------------------
//  00 nul   01 soh   02 stx   03 etx   04 eot   05 enq   06 ack   07 bel
//  08 bs    09 ht    0a nl    0b vt    0c np    0d cr    0e so    0f si
//  10 dle   11 dc1   12 dc2   13 dc3   14 dc4   15 nak   16 syn   17 etb
//  18 can   19 em    1a sub   1b esc   1c fs    1d gs    1e rs    1f us
//  20 sp    21 !     22 "     23 #     24 $     25 %     26 &     27 '
//  28 (     29 )     2a *     2b +     2c ,     2d -     2e .     2f /
//  30 0     31 1     32 2     33 3     34 4     35 5     36 6     37 7
//  38 8     39 9     3a :     3b ;     3c <     3d =     3e >     3f ?
//  40 @     41 A     42 B     43 C     44 D     45 E     46 F     47 G
//  48 H     49 I     4a J     4b K     4c L     4d M     4e N     4f O
//  50 P     51 Q     52 R     53 S     54 T     55 U     56 V     57 W
//  58 X     59 Y     5a Z     5b [     5c \     5d ]     5e ^     5f _
//  60 `     61 a     62 b     63 c     64 d     65 e     66 f     67 g
//  68 h     69 i     6a j     6b k     6c l     6d m     6e n     6f o
//  70 p     71 q     72 r     73 s     74 t     75 u     76 v     77 w
//  78 x     79 y     7a z     7b {     7c |     7d }     7e ~     7f del
//---------------------------------------------------------------------
//----- Execute a Vi Command -----------------------------------
static void do_cmd(int c)
{
 char *p, *q, *save_dot = ((*ptr_to_globals).dot );
 char buf[12];
 int dir;
 int cnt, i, j;
 int c1;
 char *orig_dot = ((*ptr_to_globals).dot );
 int allow_undo = 1;
 int undo_del = 1;
//	c1 = c; // quiet the compiler
//	cnt = yf = 0; // quiet the compiler
//	p = q = save_dot = buf; // quiet the compiler
 memset(buf, '\0', sizeof(buf));
 ((*ptr_to_globals).keep_index ) = 0;
 ((*ptr_to_globals).cmd_error ) = 0;
 show_status_line();
 // if this is a cursor key, skip these checks
 switch (c) {
  case (-2):
  case (-3):
  case (-5):
  case (-4):
  case (-6):
  case (-7):
  case (-10):
  case (-11):
  case (-9):
   goto key_cmd_mode;
 }
 if (((*ptr_to_globals).cmd_mode ) == 2) {
  //  flip-flop Insert/Replace mode
  if (c == (-8))
   goto dc_i;
  // we are 'R'eplacing the current *dot with new char
  if (*((*ptr_to_globals).dot ) == '\n') {
   // don't Replace past E-o-l
   ((*ptr_to_globals).cmd_mode ) = 1; // convert to insert
   undo_queue_commit();
  } else {
   if (1 <= c || ((unsigned char)(c) >= ' ' && (c) != 0x7f && (unsigned char)(c) != 0x9b)) {
    if (c != 27 && !((c) == ((*ptr_to_globals).term_orig ).c_cc[2] || (c) == 8 || (c) == 127))
     ((*ptr_to_globals).dot ) = yank_delete(((*ptr_to_globals).dot ), ((*ptr_to_globals).dot ), PARTIAL, YANKDEL, 1);
    ((*ptr_to_globals).dot ) = char_insert(((*ptr_to_globals).dot ), c, 2);
   }
   goto dc1;
  }
 }
 if (((*ptr_to_globals).cmd_mode ) == 1) {
  // hitting "Insert" twice means "R" replace mode
  if (c == (-8)) goto dc5;
  // insert the char c at "dot"
  if (1 <= c || ((unsigned char)(c) >= ' ' && (c) != 0x7f && (unsigned char)(c) != 0x9b)) {
   ((*ptr_to_globals).dot ) = char_insert(((*ptr_to_globals).dot ), c, 3);
  }
  goto dc1;
 }
 key_cmd_mode:
 switch (c) {
  //case 0x01:	// soh
  //case 0x09:	// ht
  //case 0x0b:	// vt
  //case 0x0e:	// so
  //case 0x0f:	// si
  //case 0x10:	// dle
  //case 0x11:	// dc1
  //case 0x13:	// dc3
  //case 0x16:	// syn
  //case 0x17:	// etb
  //case 0x18:	// can
  //case 0x1c:	// fs
  //case 0x1d:	// gs
  //case 0x1e:	// rs
  //case 0x1f:	// us
  //case '!':	// !-
  //case '#':	// #-
  //case '&':	// &-
  //case '(':	// (-
  //case ')':	// )-
  //case '*':	// *-
  //case '=':	// =-
  //case '@':	// @-
  //case 'K':	// K-
  //case 'Q':	// Q-
  //case 'S':	// S-
  //case 'V':	// V-
  //case '[':	// [-
  //case '\\':	// \-
  //case ']':	// ]-
  //case '_':	// _-
  //case '`':	// `-
  //case 'v':	// v-
 default: // unrecognized command
  buf[0] = c;
  buf[1] = '\0';
  not_implemented(buf);
  end_cmd_q(); // stop adding to q
 case 0x00: // nul- ignore
  break;
 case 2: // ctrl-B  scroll up   full screen
 case (-10): // Cursor Key Page Up
  dot_scroll(((*ptr_to_globals).rows ) - 2, -1);
  break;
 case 4: // ctrl-D  scroll down half screen
  dot_scroll((((*ptr_to_globals).rows ) - 2) / 2, 1);
  break;
 case 5: // ctrl-E  scroll down one line
  dot_scroll(1, 1);
  break;
 case 6: // ctrl-F  scroll down full screen
 case (-11): // Cursor Key Page Down
  dot_scroll(((*ptr_to_globals).rows ) - 2, 1);
  break;
 case 7: // ctrl-G  show current status
  ((*ptr_to_globals).last_status_cksum ) = 0; // force status update
  break;
 case 'h': // h- move left
 case (-5): // cursor key Left
 case 8: // ctrl-H- move left    (This may be ERASE char)
 case 0x7f: // DEL- move left   (This may be ERASE char)
  do {
   dot_left();
  } while (--((*ptr_to_globals).cmdcnt ) > 0);
  break;
 case 10: // Newline ^J
 case 'j': // j- goto next line, same col
 case (-3): // cursor key Down
 case 13: // Carriage Return ^M
 case '+': // +- goto next line
  q = ((*ptr_to_globals).dot );
  do {
   p = next_line(q);
   if (p == end_line(q)) {
    indicate_error();
    goto dc1;
   }
   q = p;
  } while (--((*ptr_to_globals).cmdcnt ) > 0);
  ((*ptr_to_globals).dot ) = q;
  if (c == 13 || c == '+') {
   dot_skip_over_ws();
  } else {
   // try to stay in saved column
   ((*ptr_to_globals).dot ) = ((*ptr_to_globals).cindex ) == C_END ? end_line(((*ptr_to_globals).dot )) : move_to_col(((*ptr_to_globals).dot ), ((*ptr_to_globals).cindex ));
   ((*ptr_to_globals).keep_index ) = 1;
  }
  break;
 case 12: // ctrl-L  force redraw whole screen
 case 18: // ctrl-R  force redraw
  redraw(1); // this will redraw the entire display
  break;
 case 21: // ctrl-U  scroll up half screen
  dot_scroll((((*ptr_to_globals).rows ) - 2) / 2, -1);
  break;
 case 25: // ctrl-Y  scroll up one line
  dot_scroll(1, -1);
  break;
 case 27: // esc
  if (((*ptr_to_globals).cmd_mode ) == 0)
   indicate_error();
  ((*ptr_to_globals).cmd_mode ) = 0; // stop inserting
  undo_queue_commit();
  end_cmd_q();
  ((*ptr_to_globals).last_status_cksum ) = 0; // force status update
  break;
 case ' ': // move right
 case 'l': // move right
 case (-4): // Cursor Key Right
  do {
   dot_right();
  } while (--((*ptr_to_globals).cmdcnt ) > 0);
  break;
 case '"': // "- name a register to use for Delete/Yank
  c1 = (get_one_char() | 0x20) - 'a'; // | 0x20 is tolower()
  if ((unsigned)c1 <= 25) { // a-z?
   ((*ptr_to_globals).YDreg ) = c1;
  } else {
   indicate_error();
  }
  break;
 case '\'': // '- goto a specific mark
  c1 = (get_one_char() | 0x20);
  if ((unsigned)(c1 - 'a') <= 25) { // a-z?
   c1 = (c1 - 'a');
   // get the b-o-l
   q = ((*ptr_to_globals).mark )[c1];
   if (((*ptr_to_globals).text ) <= q && q < ((*ptr_to_globals).end )) {
    ((*ptr_to_globals).dot ) = q;
    dot_begin(); // go to B-o-l
    dot_skip_over_ws();
   } else {
    indicate_error();
   }
  } else if (c1 == '\'') { // goto previous context
   ((*ptr_to_globals).dot ) = swap_context(((*ptr_to_globals).dot )); // swap current and previous context
   dot_begin(); // go to B-o-l
   dot_skip_over_ws();
   orig_dot = ((*ptr_to_globals).dot ); // this doesn't update stored contexts
  } else {
   indicate_error();
  }
  break;
 case 'm': // m- Mark a line
  // this is really stupid.  If there are any inserts or deletes
  // between text[0] and dot then this mark will not point to the
  // correct location! It could be off by many lines!
  // Well..., at least its quick and dirty.
  c1 = (get_one_char() | 0x20) - 'a';
  if ((unsigned)c1 <= 25) { // a-z?
   // remember the line
   ((*ptr_to_globals).mark )[c1] = ((*ptr_to_globals).dot );
  } else {
   indicate_error();
  }
  break;
 case 'P': // P- Put register before
 case 'p': // p- put register after
  p = ((*ptr_to_globals).reg )[((*ptr_to_globals).YDreg )];
  if (p == ((void*)0)) {
   status_line_bold("Nothing in register %c", what_reg());
   break;
  }
  cnt = 0;
  i = ((*ptr_to_globals).cmdcnt ) ?: 1;
  // are we putting whole lines or strings
  if (((*ptr_to_globals).regtype )[((*ptr_to_globals).YDreg )] == WHOLE) {
   if (c == 'P') {
    dot_begin(); // putting lines- Put above
   }
   else /* if ( c == 'p') */ {
    // are we putting after very last line?
    if (end_line(((*ptr_to_globals).dot )) == (((*ptr_to_globals).end ) - 1)) {
     ((*ptr_to_globals).dot ) = ((*ptr_to_globals).end ); // force dot to end of text[]
    } else {
     dot_next(); // next line, then put before
    }
   }
  } else {
   if (c == 'p')
    dot_right(); // move to right, can move to NL
   // how far to move cursor if register doesn't have a NL
   if (strchr(p, '\n') == ((void*)0))
    cnt = i * strlen(p) - 1;
  }
  do {
   // dot is adjusted if text[] is reallocated so we don't have to
   string_insert(((*ptr_to_globals).dot ), p, allow_undo); // insert the string
   allow_undo = 2;
  } while (--((*ptr_to_globals).cmdcnt ) > 0);
  ((*ptr_to_globals).dot ) += cnt;
  dot_skip_over_ws();
  yank_status("Put", p, i);
  end_cmd_q(); // stop adding to q
  break;
 case 'U': // U- Undo; replace current line with original version
  if (((*ptr_to_globals).reg )[27] != ((void*)0)) {
   p = begin_line(((*ptr_to_globals).dot ));
   q = end_line(((*ptr_to_globals).dot ));
   p = text_hole_delete(p, q, 1); // delete cur line
   p += string_insert(p, ((*ptr_to_globals).reg )[27], 2); // insert orig line
   ((*ptr_to_globals).dot ) = p;
   dot_skip_over_ws();
   yank_status("Undo", ((*ptr_to_globals).reg )[27], 1);
  }
  break;
 case 'u': // u- undo last operation
  undo_pop();
  break;
 case '$': // $- goto end of line
 case (-7): // Cursor Key End
  for (;;) {
   ((*ptr_to_globals).dot ) = end_line(((*ptr_to_globals).dot ));
   if (--((*ptr_to_globals).cmdcnt ) <= 0)
    break;
   dot_next();
  }
  ((*ptr_to_globals).cindex ) = C_END;
  ((*ptr_to_globals).keep_index ) = 1;
  break;
 case '%': // %- find matching char of pair () [] {}
  for (q = ((*ptr_to_globals).dot ); q < ((*ptr_to_globals).end ) && *q != '\n'; q++) {
   if (strchr("()[]{}", *q) != ((void*)0)) {
    // we found half of a pair
    p = find_pair(q, *q);
    if (p == ((void*)0)) {
     indicate_error();
    } else {
     ((*ptr_to_globals).dot ) = p;
    }
    break;
   }
  }
  if (*q == '\n')
   indicate_error();
  break;
 case 'f': // f- forward to a user specified char
 case 'F': // F- backward to a user specified char
 case 't': // t- move to char prior to next x
 case 'T': // T- move to char after previous x
  ((*ptr_to_globals).last_search_char ) = get_one_char(); // get the search char
  ((*ptr_to_globals).last_search_cmd ) = c;
  // fall through
 case ';': // ;- look at rest of line for last search char
 case ',': // ,- repeat latest search in opposite direction
  dot_to_char(c != ',' ? ((*ptr_to_globals).last_search_cmd ) : ((*ptr_to_globals).last_search_cmd ) ^ 0x20);
  break;
 case '.': // .- repeat the last modifying command
  // Stuff the last_modifying_cmd back into stdin
  // and let it be re-executed.
  if (((*ptr_to_globals).lmc_len ) != 0) {
   if (((*ptr_to_globals).cmdcnt )) // update saved count if current count is non-zero
    ((*ptr_to_globals).dotcnt ) = ((*ptr_to_globals).cmdcnt );
   ((*ptr_to_globals).last_modifying_cmd )[((*ptr_to_globals).lmc_len )] = '\0';
   ((*ptr_to_globals).ioq ) = ((*ptr_to_globals).ioq_start ) = xasprintf("%u%s", ((*ptr_to_globals).dotcnt ), ((*ptr_to_globals).last_modifying_cmd ));
  }
  break;
 case 'N': // N- backward search for last pattern
  dir = ((*ptr_to_globals).last_search_pattern)[0] == '/' ? BACK : FORWARD;
  goto dc4; // now search for pattern
  break;
 case '?': // ?- backward search for a pattern
 case '/': // /- forward search for a pattern
  buf[0] = c;
  buf[1] = '\0';
  q = get_input_line(buf); // get input line- use "status line"
  if (!q[0]) // user changed mind and erased the "/"-  do nothing
   break;
  if (!q[1]) { // if no pat re-use old pat
   if (((*ptr_to_globals).last_search_pattern)[0])
    ((*ptr_to_globals).last_search_pattern)[0] = c;
  } else { // strlen(q) > 1: new pat- save it and find
   free(((*ptr_to_globals).last_search_pattern));
   ((*ptr_to_globals).last_search_pattern) = xstrdup(q);
  }
  // fall through
 case 'n': // n- repeat search for last pattern
  // search rest of text[] starting at next char
  // if search fails "dot" is unchanged
  dir = ((*ptr_to_globals).last_search_pattern)[0] == '/' ? FORWARD : BACK;
 dc4:
  if (((*ptr_to_globals).last_search_pattern)[1] == '\0') {
   status_line_bold("No previous search");
   break;
  }
  do {
   q = char_search(((*ptr_to_globals).dot ) + dir, ((*ptr_to_globals).last_search_pattern) + 1,
      (dir << 1) | FULL);
   if (q != ((void*)0)) {
    ((*ptr_to_globals).dot ) = q; // good search, update "dot"
   } else {
    // no pattern found between "dot" and top/bottom of file
    // continue from other end of file
    const char *msg;
    q = char_search(dir == FORWARD ? ((*ptr_to_globals).text ) : ((*ptr_to_globals).end ) - 1,
      ((*ptr_to_globals).last_search_pattern) + 1, (dir << 1) | FULL);
    if (q != ((void*)0)) { // found something
     ((*ptr_to_globals).dot ) = q; // found new pattern- goto it
     msg = "search hit %s, continuing at %s";
    } else { // pattern is nowhere in file
     ((*ptr_to_globals).cmdcnt ) = 0; // force exit from loop
     msg = "Pattern not found";
    }
    if (dir == FORWARD)
     status_line_bold(msg, "BOTTOM", "TOP");
    else
     status_line_bold(msg, "TOP", "BOTTOM");
   }
  } while (--((*ptr_to_globals).cmdcnt ) > 0);
  break;
 case '{': // {- move backward paragraph
 case '}': // }- move forward paragraph
  dir = c == '}' ? FORWARD : BACK;
  do {
   int skip = 1; // initially skip consecutive empty lines
   while (dir == FORWARD ? ((*ptr_to_globals).dot ) < ((*ptr_to_globals).end ) - 1 : ((*ptr_to_globals).dot ) > ((*ptr_to_globals).text )) {
    if (*((*ptr_to_globals).dot ) == '\n' && ((*ptr_to_globals).dot )[dir] == '\n') {
     if (!skip) {
      if (dir == FORWARD)
       ++((*ptr_to_globals).dot ); // move to next blank line
      goto dc2;
     }
    }
    else {
     skip = 0;
    }
    ((*ptr_to_globals).dot ) += dir;
   }
   goto dc6; // end of file
 dc2: continue;
  } while (--((*ptr_to_globals).cmdcnt ) > 0);
  break;
 case '0': // 0- goto beginning of line
 case '1': // 1-
 case '2': // 2-
 case '3': // 3-
 case '4': // 4-
 case '5': // 5-
 case '6': // 6-
 case '7': // 7-
 case '8': // 8-
 case '9': // 9-
  if (c == '0' && ((*ptr_to_globals).cmdcnt ) < 1) {
   dot_begin(); // this was a standalone zero
  } else {
   ((*ptr_to_globals).cmdcnt ) = ((*ptr_to_globals).cmdcnt ) * 10 + (c - '0'); // this 0 is part of a number
  }
  break;
 case ':': // :- the colon mode commands
  p = get_input_line(":"); // get input line- use "status line"
  colon(p); // execute the command
  break;
 case '<': // <- Left  shift something
 case '>': // >- Right shift something
  cnt = count_lines(((*ptr_to_globals).text ), ((*ptr_to_globals).dot )); // remember what line we are on
  if (find_range(&p, &q, c) == -1)
   goto dc6;
  i = count_lines(p, q); // # of lines we are shifting
  for (p = begin_line(p); i > 0; i--, p = next_line(p)) {
   if (c == '<') {
    // shift left- remove tab or tabstop spaces
    if (*p == '\t') {
     // shrink buffer 1 char
     text_hole_delete(p, p, allow_undo);
    } else if (*p == ' ') {
     // we should be calculating columns, not just SPACE
     for (j = 0; *p == ' ' && j < ((*ptr_to_globals).tabstop ); j++) {
      text_hole_delete(p, p, allow_undo);
      allow_undo = 2;
     }
    }
   } else if (/* c == '>' && */ p != end_line(p)) {
    // shift right -- add tab or tabstop spaces on non-empty lines
    char_insert(p, '\t', allow_undo);
   }
   allow_undo = 2;
  }
  ((*ptr_to_globals).dot ) = find_line(cnt); // what line were we on
  dot_skip_over_ws();
  end_cmd_q(); // stop adding to q
  break;
 case 'A': // A- append at e-o-l
  dot_end(); // go to e-o-l
  //**** fall through to ... 'a'
 case 'a': // a- append after current char
  if (*((*ptr_to_globals).dot ) != '\n')
   ((*ptr_to_globals).dot )++;
  goto dc_i;
  break;
 case 'B': // B- back a blank-delimited Word
 case 'E': // E- end of a blank-delimited word
 case 'W': // W- forward a blank-delimited word
  dir = FORWARD;
  if (c == 'B')
   dir = BACK;
  do {
   if (c == 'W' || ((*__ctype_b_loc ())[(int) ((((*ptr_to_globals).dot )[dir]))] & (unsigned short int) _ISspace)) {
    ((*ptr_to_globals).dot ) = skip_thing(((*ptr_to_globals).dot ), 1, dir, S_TO_WS);
    ((*ptr_to_globals).dot ) = skip_thing(((*ptr_to_globals).dot ), 2, dir, S_OVER_WS);
   }
   if (c != 'W')
    ((*ptr_to_globals).dot ) = skip_thing(((*ptr_to_globals).dot ), 1, dir, S_BEFORE_WS);
  } while (--((*ptr_to_globals).cmdcnt ) > 0);
  break;
 case 'C': // C- Change to e-o-l
 case 'D': // D- delete to e-o-l
  save_dot = ((*ptr_to_globals).dot );
  ((*ptr_to_globals).dot ) = dollar_line(((*ptr_to_globals).dot )); // move to before NL
  // copy text into a register and delete
  ((*ptr_to_globals).dot ) = yank_delete(save_dot, ((*ptr_to_globals).dot ), PARTIAL, YANKDEL, 1); // delete to e-o-l
  if (c == 'C')
   goto dc_i; // start inserting
  if (c == 'D')
   end_cmd_q(); // stop adding to q
  break;
 case 'g': // 'gg' goto a line number (vim) (default: very first line)
  c1 = get_one_char();
  if (c1 != 'g') {
   buf[0] = 'g';
   // c1 < 0 if the key was special. Try "g<up-arrow>"
   // TODO: if Unicode?
   buf[1] = (c1 >= 0 ? c1 : '*');
   buf[2] = '\0';
   not_implemented(buf);
   ((*ptr_to_globals).cmd_error ) = 1;
   break;
  }
  if (((*ptr_to_globals).cmdcnt ) == 0)
   ((*ptr_to_globals).cmdcnt ) = 1;
  // fall through
 case 'G': // G- goto to a line number (default= E-O-F)
  ((*ptr_to_globals).dot ) = ((*ptr_to_globals).end ) - 1; // assume E-O-F
  if (((*ptr_to_globals).cmdcnt ) > 0) {
   ((*ptr_to_globals).dot ) = find_line(((*ptr_to_globals).cmdcnt )); // what line is #cmdcnt
  }
  dot_begin();
  dot_skip_over_ws();
  break;
 case 'H': // H- goto top line on screen
  ((*ptr_to_globals).dot ) = ((*ptr_to_globals).screenbegin );
  if (((*ptr_to_globals).cmdcnt ) > (((*ptr_to_globals).rows ) - 1)) {
   ((*ptr_to_globals).cmdcnt ) = (((*ptr_to_globals).rows ) - 1);
  }
  while (--((*ptr_to_globals).cmdcnt ) > 0) {
   dot_next();
  }
  dot_begin();
  dot_skip_over_ws();
  break;
 case 'I': // I- insert before first non-blank
  dot_begin(); // 0
  dot_skip_over_ws();
  //**** fall through to ... 'i'
 case 'i': // i- insert before current char
 case (-8): // Cursor Key Insert
 dc_i:
  ((*ptr_to_globals).newindent ) = -1;
  ((*ptr_to_globals).cmd_mode ) = 1; // start inserting
  undo_queue_commit(); // commit queue when cmd_mode changes
  break;
 case 'J': // J- join current and next lines together
  do {
   dot_end(); // move to NL
   if (((*ptr_to_globals).dot ) < ((*ptr_to_globals).end ) - 1) { // make sure not last char in text[]
    undo_push(((*ptr_to_globals).dot ), 1, 1);
    *((*ptr_to_globals).dot )++ = ' '; // replace NL with space
    undo_push((((*ptr_to_globals).dot ) - 1), 1, 2);
    while (((*__ctype_b_loc ())[(int) ((*((*ptr_to_globals).dot )))] & (unsigned short int) _ISblank)) { // delete leading WS
     text_hole_delete(((*ptr_to_globals).dot ), ((*ptr_to_globals).dot ), 2);
    }
   }
  } while (--((*ptr_to_globals).cmdcnt ) > 0);
  end_cmd_q(); // stop adding to q
  break;
 case 'L': // L- goto bottom line on screen
  ((*ptr_to_globals).dot ) = end_screen();
  if (((*ptr_to_globals).cmdcnt ) > (((*ptr_to_globals).rows ) - 1)) {
   ((*ptr_to_globals).cmdcnt ) = (((*ptr_to_globals).rows ) - 1);
  }
  while (--((*ptr_to_globals).cmdcnt ) > 0) {
   dot_prev();
  }
  dot_begin();
  dot_skip_over_ws();
  break;
 case 'M': // M- goto middle line on screen
  ((*ptr_to_globals).dot ) = ((*ptr_to_globals).screenbegin );
  for (cnt = 0; cnt < (((*ptr_to_globals).rows )-1) / 2; cnt++)
   ((*ptr_to_globals).dot ) = next_line(((*ptr_to_globals).dot ));
  dot_skip_over_ws();
  break;
 case 'O': // O- open an empty line above
  dot_begin();
  // special case: use indent of current line
  ((*ptr_to_globals).newindent ) = get_column(((*ptr_to_globals).dot ) + indent_len(((*ptr_to_globals).dot )));
  goto dc3;
 case 'o': // o- open an empty line below
  dot_end();
 dc3:
  ((*ptr_to_globals).cmd_mode ) = 1; // switch to insert mode early
  ((*ptr_to_globals).dot ) = char_insert(((*ptr_to_globals).dot ), '\n', 1);
  if (c == 'O' && !(((*ptr_to_globals).vi_setops ) & (1 << 0))) {
   // done in char_insert() for 'O'+autoindent
   dot_prev();
  }
  goto dc_i;
  break;
 case 'R': // R- continuous Replace char
 dc5:
  ((*ptr_to_globals).cmd_mode ) = 2;
  undo_queue_commit();
  ((*ptr_to_globals).rstart ) = ((*ptr_to_globals).dot );
  break;
 case (-9):
  if (((*ptr_to_globals).dot ) < ((*ptr_to_globals).end ) - 1)
   ((*ptr_to_globals).dot ) = yank_delete(((*ptr_to_globals).dot ), ((*ptr_to_globals).dot ), PARTIAL, YANKDEL, 1);
  break;
 case 'X': // X- delete char before dot
 case 'x': // x- delete the current char
 case 's': // s- substitute the current char
  dir = 0;
  if (c == 'X')
   dir = -1;
  do {
   if (((*ptr_to_globals).dot )[dir] != '\n') {
    if (c == 'X')
     ((*ptr_to_globals).dot )--; // delete prev char
    ((*ptr_to_globals).dot ) = yank_delete(((*ptr_to_globals).dot ), ((*ptr_to_globals).dot ), PARTIAL, YANKDEL, allow_undo); // delete char
    allow_undo = 2;
   }
  } while (--((*ptr_to_globals).cmdcnt ) > 0);
  end_cmd_q(); // stop adding to q
  if (c == 's')
   goto dc_i; // start inserting
  break;
 case 'Z': // Z- if modified, {write}; exit
  c1 = get_one_char();
  // ZQ means to exit without saving
  if (c1 == 'Q') {
   ((*ptr_to_globals).editing ) = 0;
   optind = ((*ptr_to_globals).cmdline_filecnt );
   break;
  }
  // ZZ means to save file (if necessary), then exit
  if (c1 != 'Z') {
   indicate_error();
   break;
  }
  if (((*ptr_to_globals).modified_count )) {
   if (1 && ((*ptr_to_globals).readonly_mode ) && ((*ptr_to_globals).current_filename )) {
    status_line_bold("'%s' is read only", ((*ptr_to_globals).current_filename ));
    break;
   }
   cnt = file_write(((*ptr_to_globals).current_filename ), ((*ptr_to_globals).text ), ((*ptr_to_globals).end ) - 1);
   if (cnt < 0) {
    if (cnt == -1)
     status_line_bold("Write error: ""%s" , strerror((*__errno_location ())));
   } else if (cnt == (((*ptr_to_globals).end ) - 1 - ((*ptr_to_globals).text ) + 1)) {
    ((*ptr_to_globals).editing ) = 0;
   }
  } else {
   ((*ptr_to_globals).editing ) = 0;
  }
  // are there other files to edit?
  j = ((*ptr_to_globals).cmdline_filecnt ) - optind - 1;
  if (((*ptr_to_globals).editing ) == 0 && j > 0) {
   ((*ptr_to_globals).editing ) = 1;
   ((*ptr_to_globals).modified_count ) = 0;
   ((*ptr_to_globals).last_modified_count) = -1;
   status_line_bold("%u more file(s) to edit", j);
  }
  break;
 case '^': // ^- move to first non-blank on line
  dot_begin();
  dot_skip_over_ws();
  break;
 case 'b': // b- back a word
 case 'e': // e- end of word
  dir = FORWARD;
  if (c == 'b')
   dir = BACK;
  do {
   if ((((*ptr_to_globals).dot ) + dir) < ((*ptr_to_globals).text ) || (((*ptr_to_globals).dot ) + dir) > ((*ptr_to_globals).end ) - 1)
    break;
   ((*ptr_to_globals).dot ) += dir;
   if (((*__ctype_b_loc ())[(int) ((*((*ptr_to_globals).dot )))] & (unsigned short int) _ISspace)) {
    ((*ptr_to_globals).dot ) = skip_thing(((*ptr_to_globals).dot ), (c == 'e') ? 2 : 1, dir, S_OVER_WS);
   }
   if (((*__ctype_b_loc ())[(int) ((*((*ptr_to_globals).dot )))] & (unsigned short int) _ISalnum) || *((*ptr_to_globals).dot ) == '_') {
    ((*ptr_to_globals).dot ) = skip_thing(((*ptr_to_globals).dot ), 1, dir, S_END_ALNUM);
   } else if (((*__ctype_b_loc ())[(int) ((*((*ptr_to_globals).dot )))] & (unsigned short int) _ISpunct)) {
    ((*ptr_to_globals).dot ) = skip_thing(((*ptr_to_globals).dot ), 1, dir, S_END_PUNCT);
   }
  } while (--((*ptr_to_globals).cmdcnt ) > 0);
  break;
 case 'c': // c- change something
 case 'd': // d- delete something
 case 'y': // y- yank   something
 case 'Y': // Y- Yank a line
 {
  int yf = YANKDEL; // assume either "c" or "d"
  int buftype;
  char *savereg = ((*ptr_to_globals).reg )[((*ptr_to_globals).YDreg )];
  if (c == 'y' || c == 'Y')
   yf = YANKONLY;
  // determine range, and whether it spans lines
  buftype = find_range(&p, &q, c);
  if (buftype == -1) // invalid range
   goto dc6;
  if (buftype == WHOLE) {
   save_dot = p; // final cursor position is start of range
   p = begin_line(p);
   if (c == 'c') // special case: use indent of current line
    ((*ptr_to_globals).newindent ) = get_column(p + indent_len(p));
   q = end_line(q);
  }
  ((*ptr_to_globals).dot ) = yank_delete(p, q, buftype, yf, 1); // delete word
  if (buftype == WHOLE) {
   if (c == 'c') {
    ((*ptr_to_globals).cmd_mode ) = 1; // switch to insert mode early
    ((*ptr_to_globals).dot ) = char_insert(((*ptr_to_globals).dot ), '\n', 2);
    // on the last line of file don't move to prev line,
    // handled in char_insert() if autoindent is enabled
    if (((*ptr_to_globals).dot ) != (((*ptr_to_globals).end )-1) && !(((*ptr_to_globals).vi_setops ) & (1 << 0))) {
     dot_prev();
    }
   } else if (c == 'd') {
    dot_begin();
    dot_skip_over_ws();
   } else {
    ((*ptr_to_globals).dot ) = save_dot;
   }
  }
  // if CHANGING, not deleting, start inserting after the delete
  if (c == 'c') {
   goto dc_i; // start inserting
  }
  // only update status if a yank has actually happened
  if (((*ptr_to_globals).reg )[((*ptr_to_globals).YDreg )] != savereg)
   yank_status(c == 'd' ? "Delete" : "Yank", ((*ptr_to_globals).reg )[((*ptr_to_globals).YDreg )], 1);
 dc6:
  end_cmd_q(); // stop adding to q
  break;
 }
 case 'k': // k- goto prev line, same col
 case (-2): // cursor key Up
 case '-': // -- goto prev line
  q = ((*ptr_to_globals).dot );
  do {
   p = prev_line(q);
   if (p == begin_line(q)) {
    indicate_error();
    goto dc1;
   }
   q = p;
  } while (--((*ptr_to_globals).cmdcnt ) > 0);
  ((*ptr_to_globals).dot ) = q;
  if (c == '-') {
   dot_skip_over_ws();
  } else {
   // try to stay in saved column
   ((*ptr_to_globals).dot ) = ((*ptr_to_globals).cindex ) == C_END ? end_line(((*ptr_to_globals).dot )) : move_to_col(((*ptr_to_globals).dot ), ((*ptr_to_globals).cindex ));
   ((*ptr_to_globals).keep_index ) = 1;
  }
  break;
 case 'r': // r- replace the current char with user input
  c1 = get_one_char(); // get the replacement char
  if (c1 != 27) {
   if (end_line(((*ptr_to_globals).dot )) - ((*ptr_to_globals).dot ) < (((*ptr_to_globals).cmdcnt ) ?: 1)) {
    indicate_error();
    goto dc6;
   }
   do {
    ((*ptr_to_globals).dot ) = text_hole_delete(((*ptr_to_globals).dot ), ((*ptr_to_globals).dot ), allow_undo);
    allow_undo = 2;
    ((*ptr_to_globals).dot ) = char_insert(((*ptr_to_globals).dot ), c1, allow_undo);
   } while (--((*ptr_to_globals).cmdcnt ) > 0);
   dot_left();
  }
  end_cmd_q(); // stop adding to q
  break;
 case 'w': // w- forward a word
  do {
   if (((*__ctype_b_loc ())[(int) ((*((*ptr_to_globals).dot )))] & (unsigned short int) _ISalnum) || *((*ptr_to_globals).dot ) == '_') { // we are on ALNUM
    ((*ptr_to_globals).dot ) = skip_thing(((*ptr_to_globals).dot ), 1, FORWARD, S_END_ALNUM);
   } else if (((*__ctype_b_loc ())[(int) ((*((*ptr_to_globals).dot )))] & (unsigned short int) _ISpunct)) { // we are on PUNCT
    ((*ptr_to_globals).dot ) = skip_thing(((*ptr_to_globals).dot ), 1, FORWARD, S_END_PUNCT);
   }
   if (((*ptr_to_globals).dot ) < ((*ptr_to_globals).end ) - 1)
    ((*ptr_to_globals).dot )++; // move over word
   if (((*__ctype_b_loc ())[(int) ((*((*ptr_to_globals).dot )))] & (unsigned short int) _ISspace)) {
    ((*ptr_to_globals).dot ) = skip_thing(((*ptr_to_globals).dot ), 2, FORWARD, S_OVER_WS);
   }
  } while (--((*ptr_to_globals).cmdcnt ) > 0);
  break;
 case 'z': // z-
  c1 = get_one_char(); // get the replacement char
  cnt = 0;
  if (c1 == '.')
   cnt = (((*ptr_to_globals).rows ) - 2) / 2; // put dot at center
  if (c1 == '-')
   cnt = ((*ptr_to_globals).rows ) - 2; // put dot at bottom
  ((*ptr_to_globals).screenbegin ) = begin_line(((*ptr_to_globals).dot )); // start dot at top
  dot_scroll(cnt, -1);
  break;
 case '|': // |- move to column "cmdcnt"
  ((*ptr_to_globals).dot ) = move_to_col(((*ptr_to_globals).dot ), ((*ptr_to_globals).cmdcnt ) - 1); // try to move to column
  break;
 case '~': // ~- flip the case of letters   a-z -> A-Z
  do {
   if (((*__ctype_b_loc ())[(int) ((*((*ptr_to_globals).dot )))] & (unsigned short int) _ISalpha)) {
    undo_push(((*ptr_to_globals).dot ), 1, undo_del);
    *((*ptr_to_globals).dot ) = ((*__ctype_b_loc ())[(int) ((*((*ptr_to_globals).dot )))] & (unsigned short int) _ISlower) ? toupper(*((*ptr_to_globals).dot )) : tolower(*((*ptr_to_globals).dot ));
    undo_push(((*ptr_to_globals).dot ), 1, 2);
    undo_del = 3;
   }
   dot_right();
  } while (--((*ptr_to_globals).cmdcnt ) > 0);
  end_cmd_q(); // stop adding to q
  break;
  //----- The Cursor and Function Keys -----------------------------
 case (-6): // Cursor Key Home
  dot_begin();
  break;
  // The Fn keys could point to do_macro which could translate them
 }
 dc1:
 // if text[] just became empty, add back an empty line
 if (((*ptr_to_globals).end ) == ((*ptr_to_globals).text )) {
  char_insert(((*ptr_to_globals).text ), '\n', 0); // start empty buf with dummy line
  ((*ptr_to_globals).dot ) = ((*ptr_to_globals).text );
 }
 // it is OK for dot to exactly equal to end, otherwise check dot validity
 if (((*ptr_to_globals).dot ) != ((*ptr_to_globals).end )) {
  ((*ptr_to_globals).dot ) = bound_dot(((*ptr_to_globals).dot )); // make sure "dot" is valid
 }
 if (((*ptr_to_globals).dot ) != orig_dot)
  check_context(c); // update the current context
 if (!((*__ctype_b_loc ())[(int) ((c))] & (unsigned short int) _ISdigit))
  ((*ptr_to_globals).cmdcnt ) = 0; // cmd was not a number, reset cmdcnt
 cnt = ((*ptr_to_globals).dot ) - begin_line(((*ptr_to_globals).dot ));
 // Try to stay off of the Newline
 if (*((*ptr_to_globals).dot ) == '\n' && cnt > 0 && ((*ptr_to_globals).cmd_mode ) == 0)
  ((*ptr_to_globals).dot )--;
}
// NB!  the CRASHME code is unmaintained, and doesn't currently build
static void run_cmds(char *p)
{
 while (p) {
  char *q = p;
  p = strchr(q, '\n');
  if (p)
   while (*p == '\n')
    *p++ = '\0';
  colon(q);
 }
}
static void edit_file(char *fn)
{
 int c;
 int sig;
 ((*ptr_to_globals).editing ) = 1; // 0 = exit, 1 = one file, 2 = multiple files
 rawmode();
 ((*ptr_to_globals).rows ) = 24;
 ((*ptr_to_globals).columns ) = 80;
 (*ptr_to_globals).get_rowcol_error = query_screen_dimensions();
 if ((*ptr_to_globals).get_rowcol_error /* TODO? && no input on stdin */) {
  uint64_t k;
  write1("\033""[999;999H" "\033""[6n");
  fflush_all();
  k = safe_read_key(0, ((*ptr_to_globals).readbuffer ), /*timeout_ms:*/ 100);
  if ((int32_t)k == (-0x100)) {
   uint32_t rc = (k >> 32);
   ((*ptr_to_globals).columns ) = (rc & 0x7fff);
   if (((*ptr_to_globals).columns ) > MAX_SCR_COLS)
    ((*ptr_to_globals).columns ) = MAX_SCR_COLS;
   ((*ptr_to_globals).rows ) = ((rc >> 16) & 0x7fff);
   if (((*ptr_to_globals).rows ) > MAX_SCR_ROWS)
    ((*ptr_to_globals).rows ) = MAX_SCR_ROWS;
  }
 }
 new_screen(((*ptr_to_globals).rows ), ((*ptr_to_globals).columns )); // get memory for virtual screen
 init_text_buffer(fn);
 ((*ptr_to_globals).YDreg ) = 26; // default Yank/Delete reg
//	Ureg = 27; - const		// hold orig line for "U" cmd
 ((*ptr_to_globals).mark )[26] = ((*ptr_to_globals).mark )[27] = ((*ptr_to_globals).text ); // init "previous context"
 ((*ptr_to_globals).crow ) = 0;
 ((*ptr_to_globals).ccol ) = 0;
 signal(28, winch_handler);
 signal(20, tstp_handler);
 sig = __sigsetjmp (((*ptr_to_globals).restart ), 1);
 if (sig != 0) {
  ((*ptr_to_globals).screenbegin ) = ((*ptr_to_globals).dot ) = ((*ptr_to_globals).text );
 }
 // int_handler() can jump to "restart",
 // must install handler *after* initializing "restart"
 signal(2, int_handler);
 ((*ptr_to_globals).cmd_mode ) = 0; // 0=command  1=insert  2='R'eplace
 ((*ptr_to_globals).cmdcnt ) = 0;
 ((*ptr_to_globals).offset ) = 0; // no horizontal offset
 free(((*ptr_to_globals).ioq_start ));
 ((*ptr_to_globals).ioq_start ) = ((void*)0);
 ((*ptr_to_globals).adding2q ) = 0;
 while (((*ptr_to_globals).initial_cmds ))
  run_cmds((char *)llist_pop(&((*ptr_to_globals).initial_cmds )));
 redraw(0); // dont force every col re-draw
 //------This is the main Vi cmd handling loop -----------------------
 while (((*ptr_to_globals).editing ) > 0) {
  c = get_one_char(); // get a cmd from user
  // save a copy of the current line- for the 'U" command
  if (begin_line(((*ptr_to_globals).dot )) != ((*ptr_to_globals).edit_file__cur_line)) {
   ((*ptr_to_globals).edit_file__cur_line) = begin_line(((*ptr_to_globals).dot ));
   text_yank(begin_line(((*ptr_to_globals).dot )), end_line(((*ptr_to_globals).dot )), 27, PARTIAL);
  }
  // If c is a command that changes text[],
  // (re)start remembering the input for the "." command.
  if (!((*ptr_to_globals).adding2q )
   && ((*ptr_to_globals).ioq_start ) == ((void*)0)
   && ((*ptr_to_globals).cmd_mode ) == 0 // command mode
   && c > '\0' // exclude NUL and non-ASCII chars
   && c < 0x7f // (Unicode and such)
   && strchr(modifying_cmds, c)
  ) {
   start_new_cmd_q(c);
  }
  do_cmd(c); // execute the user command
  // poll to see if there is input already waiting. if we are
  // not able to display output fast enough to keep up, skip
  // the display update until we catch up with input.
  if (!((*ptr_to_globals).readbuffer )[0] && mysleep(0) == 0) {
   // no input pending - so update output
   refresh(0);
   show_status_line();
  }
 }
 //-------------------------------------------------------------------
 go_bottom_and_clear_to_eol();
 cookmode();
}
enum {
 OPTBIT_c,
 OPTBIT_H,
 OPTBIT_h,
 OPTBIT_R,
 OPT_C = + 0,
 OPT_c = (1 << OPTBIT_c) + 0,
 OPT_H = 1 << OPTBIT_H,
 OPT_h = 1 << OPTBIT_h,
 OPT_R = (1 << OPTBIT_R) + 0,
};
int main(int argc, char **argv) ;
int main(int argc, char **argv)
{
 int opts;
 standalone_argc = argc;
 do { (ptr_to_globals = (xzalloc(sizeof((*ptr_to_globals))))); ((*ptr_to_globals).last_modified_count)--; ((*ptr_to_globals).last_search_pattern) = xzalloc(2); ((*ptr_to_globals).tabstop ) = 8; ((*ptr_to_globals).newindent )--; } while (0);
 //undo_stack_tail = NULL; - already is
 ((*ptr_to_globals).undo_queue_state) = 64;
 //undo_q = 0; - already is
 // 0: all of our options are disabled by default in vim
 //vi_setops = 0;
 opts = getopt32(argv, "c:*" "Hh" "R" , &((*ptr_to_globals).initial_cmds ));
 if (opts & OPT_R)
  ((((*ptr_to_globals).readonly_mode )) |= 0x02);
 if (opts & OPT_H)
  show_help();
 if (opts & (OPT_H | OPT_h)) {
  vi_show_usage();
  return 1;
 }
 argv += optind;
 ((*ptr_to_globals).cmdline_filecnt ) = argc - optind;
 //  1-  process EXINIT variable from environment
 //  2-  if EXINIT is unset process $HOME/.exrc file
 //  3-  process command line args
 {
  const char *exinit = getenv("EXINIT");
  char *cmds = ((void*)0);
  if (exinit) {
   cmds = xstrdup(exinit);
  } else {
   const char *home = getenv("HOME");
   if (home && *home) {
    char *exrc = concat_path_file(home, ".exrc");
    struct stat st;
    // .exrc must belong to and only be writable by user
    if (stat(exrc, &st) == 0) {
     if ((st.st_mode & ((0200 >> 3)|((0200 >> 3) >> 3))) == 0
      && st.st_uid == getuid()
     ) {
      cmds = xmalloc_open_read_close(exrc, ((void*)0));
     } else {
      status_line_bold(".exrc: permission denied");
     }
    }
    free(exrc);
   }
  }
  if (cmds) {
   init_text_buffer(((void*)0));
   run_cmds(cmds);
   free(cmds);
  }
 }
 // "Save cursor, use alternate screen buffer, clear screen"
 write1("\033""[?1049h");
 // This is the main file handling loop
 optind = 0;
 while (1) {
  edit_file(argv[optind]); // might be NULL on 1st iteration
  // NB: optind can be changed by ":next" and ":rewind" commands
  optind++;
  if (optind >= ((*ptr_to_globals).cmdline_filecnt ))
   break;
 }
 // "Use normal screen buffer, restore cursor"
 write1("\033""[?1049l");
 return 0;
}
