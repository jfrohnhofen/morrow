//! HTML rendering.
//!
//! Everything writes into a [`core::fmt::Write`], so the firmware can render
//! straight into a socket buffer and the dev server into a `String`.
//!
//! The pages are deliberately plain: no JavaScript, no external assets, forms
//! posted the 1997 way. That is not nostalgia. The provisioning page is shown
//! inside iOS's Captive Network Assistant, a cut-down WebKit view where script
//! and fetch cannot be relied on, and the device has no route to a CDN anyway.
//!
//! The styling mirrors the OLED: one bit of colour, sharp corners, monospace,
//! and selection shown by inverting — the same gesture `Display::invert` makes
//! on the panel.

use core::fmt::{Result, Write};

use crate::digits::{DIGIT_H, DIGIT_W, DIGITS};
use crate::{Setup, Status};

/// Writes `s` with HTML metacharacters escaped.
///
/// Applied to every value that came from the network — SSIDs and timezone rules
/// especially, which are attacker-controlled in the sense that whoever can
/// reach the form chooses them.
pub fn escape<W: Write>(w: &mut W, s: &str) -> Result {
    for c in s.chars() {
        match c {
            '&' => w.write_str("&amp;")?,
            '<' => w.write_str("&lt;")?,
            '>' => w.write_str("&gt;")?,
            '"' => w.write_str("&quot;")?,
            '\'' => w.write_str("&#39;")?,
            c => w.write_char(c)?,
        }
    }
    Ok(())
}

/// The stylesheet, inlined because the device serves no second request.
///
/// Two ordering dependencies, since neither rule uses `!important`: `[hidden]`
/// comes after `.clock` so hiding the alarm clock beats its own `display`, and
/// `button:disabled` comes after `button.ghost` so a disabled ghost button
/// still reads as disabled.
const CSS: &str = "\
*{box-sizing:border-box;margin:0;padding:0}\
:root{color-scheme:dark;--fg:#fff;--bg:#000;--dim:#707070}\
body{background:var(--bg);color:var(--fg);padding:14px;max-width:32rem;margin:0 auto;\
font:16px/1.5 ui-monospace,SFMono-Regular,Menlo,Consolas,monospace;-webkit-text-size-adjust:100%}\
header{display:flex;align-items:baseline;gap:.6em;margin-bottom:14px}\
h1{font-size:1rem;letter-spacing:.34em;text-transform:uppercase}\
.ver{color:var(--dim);font-size:.75rem;margin-left:auto}\
section{border:2px solid var(--fg);margin-bottom:14px}\
h2{background:var(--fg);color:var(--bg);font-size:.75rem;letter-spacing:.24em;\
text-transform:uppercase;padding:5px 10px}\
.body{padding:12px 10px}\
.date{text-align:center;font-size:1.05rem;color:var(--dim);letter-spacing:.2em;margin:6px 0 10px}\
.clock{display:block;width:100%;max-width:17rem;margin:10px auto 20px;height:auto;\
image-rendering:pixelated}\
[hidden]{display:none}\
.none{text-align:center;color:var(--dim);padding:14px 0;letter-spacing:.1em}\
label:not(.net):not(.file-lbl),.lbl{display:block;font-size:.72rem;letter-spacing:.18em;\
text-transform:uppercase;color:var(--dim);margin:12px 0 4px}\
label:not(.net):not(.file-lbl):first-child,.lbl:first-child{margin-top:0}\
.row .lbl{margin-bottom:1px;line-height:1.2}\
.val{overflow-wrap:anywhere}\
input,select{width:100%;min-height:44px;background:var(--bg);color:var(--fg);\
border:2px solid var(--fg);border-radius:0;padding:9px;font:inherit;appearance:none;-webkit-appearance:none}\
input[type=password]{letter-spacing:.28em}\
input:focus,select:focus{outline:3px solid var(--fg);outline-offset:-1px}\
option{background:var(--bg);color:var(--fg)}\
option:checked{background:var(--fg);color:var(--bg)}\
#mo{text-align:center;text-align-last:center;-webkit-text-align-last:center}\
#mo option{text-align:center}\
input[readonly]{background:#111;color:var(--dim);border-color:#444;cursor:default}\
.clock-spin{display:flex;align-items:center;justify-content:center;gap:6px;margin:12px auto 16px}\
.clock-spin.off{opacity:.25}\
.clock-col{display:flex;flex-direction:column;align-items:center;width:76px;gap:8px}\
.clock-digits{display:block;width:100%;height:auto;image-rendering:pixelated}\
.clock-colon{display:block;width:18px;height:auto;image-rendering:pixelated}\
.spin-btn,button.spin-btn{width:100%;height:28px;min-height:28px;max-height:28px;margin:0;padding:0;\
background:none;color:var(--dim);border:none;display:flex;align-items:center;justify-content:center;\
cursor:pointer;-webkit-appearance:none;appearance:none;user-select:none}\
button.spin-btn:hover,button.spin-btn:active{background:none;color:var(--fg)}\
button.spin-btn:focus{outline:none}\
.spin-btn:disabled,button.spin-btn:disabled{opacity:.2;pointer-events:none;cursor:default}\
.file-row{display:flex;gap:8px;margin-top:14px;align-items:center}\
.file-row:first-child{margin-top:0}\
.file-lbl{flex:1;min-width:0;color:var(--dim);cursor:pointer;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;margin:0;padding:10px 11px;font:inherit;font-size:1rem;letter-spacing:normal}\
.file-row .browse-btn{width:auto;min-width:84px;height:auto;min-height:44px;margin-top:0;padding:0 12px;background:var(--bg);color:var(--fg);border:2px solid var(--fg)}\
.file-row .browse-btn:hover,.file-row .browse-btn:active{background:var(--fg);color:var(--bg)}\
.row{display:flex;gap:8px;margin-top:14px;align-items:flex-start}\
.row>*{flex:1 1 0;min-width:0;width:0;margin:0}\
button,.button{appearance:none;-webkit-appearance:none;box-sizing:border-box;display:flex;\
align-items:center;justify-content:center;text-align:center;text-decoration:none;width:100%;\
height:46px;min-height:46px;max-height:46px;line-height:1;margin-top:14px;background:var(--bg);\
color:var(--fg);border:2px solid var(--fg);border-radius:0;padding:0;font:inherit;font-weight:700;\
letter-spacing:.2em;text-transform:uppercase;cursor:pointer;vertical-align:middle}\
button:hover,.button:hover{background:var(--fg);color:var(--bg)}\
button:active,.button:active{background:var(--fg);color:var(--bg)}\
button.ghost,.button.ghost{background:var(--bg);color:var(--fg)}\
button.ghost:hover,.button.ghost:hover{background:var(--fg);color:var(--bg)}\
button.ghost:active,.button.ghost:active{background:var(--fg);color:var(--bg)}\
button:disabled,.button:disabled,form:invalid button[type=submit]{background:var(--bg);color:var(--dim);border-color:var(--dim);cursor:default;pointer-events:none}\
.row button,.row .button{margin-top:0}\
.note{font-size:.74rem;color:var(--dim);margin-top:9px;line-height:1.45}\
.net{display:flex;align-items:center;justify-content:space-between;gap:10px;\
padding:11px 6px;margin:0 -2px;border-bottom:1px solid #333;font-size:.86rem;cursor:pointer;text-decoration:none}\
.net:last-child{border-bottom:0}\
.net:hover,.net:active{background:var(--fg);color:var(--bg)}\
.net:hover .lbl,.net:active .lbl{color:var(--bg)}\
.net .lbl{margin:0}\
.net .who{display:flex;align-items:center;min-width:0;overflow-wrap:anywhere}\
.ctr{text-align:center;padding:18px 10px}\
.warn{border:2px solid var(--fg);padding:9px;margin-bottom:12px;font-size:.8rem}\
a{color:var(--fg)}\
";

/// Writes the document head and opening body.
///
/// `digits` emits the glyph sprite, which is only worth its bytes on pages that
/// actually draw a clock.
fn head<W: Write>(
    w: &mut W,
    title: &str,
    version: &str,
    digits: bool,
    refresh: Option<u8>,
) -> Result {
    write!(
        w,
        "<!doctype html><html lang=en><head><meta charset=utf-8>\
         <meta name=viewport content=\"width=device-width,initial-scale=1\">"
    )?;
    // Only the scanning page uses this, and only to come back once. A standing
    // refresh on the setup page would wipe a half-typed passphrase.
    if let Some(secs) = refresh {
        write!(w, "<meta http-equiv=refresh content=\"{secs};url=/\">")?;
    }
    w.write_str("<title>")?;
    escape(w, title)?;
    write!(w, "</title><style>{CSS}</style></head><body><header><h1>morrow</h1>\
               <span class=ver>")?;
    if !version.starts_with('v') {
        w.write_char('v')?;
    }
    escape(w, version)?;
    w.write_str("</span></header>")?;
    if digits {
        sprite(w)?;
    }
    Ok(())
}

/// Closes the document.
fn foot<W: Write>(w: &mut W) -> Result {
    w.write_str("</body></html>")
}

/// Emits the ten digit glyphs once per page, for `<use>` to reference.
///
/// These are the panel's own bitmaps, so the browser shows the same numerals
/// the OLED does. They go in a zero-sized SVG at the top of the body and every
/// clock on the page references them: inlining the paths into each clock
/// instead cost about 3 KB per page, which matters when the device renders into
/// a fixed buffer.
fn sprite<W: Write>(w: &mut W) -> Result {
    w.write_str(
        "<svg width=\"0\" height=\"0\" style=\"position:absolute\" aria-hidden=\"true\"><defs>",
    )?;
    for (n, glyph) in DIGITS.iter().enumerate() {
        write!(w, "<path id=\"d{n}\" d=\"")?;
        for (y, bits) in glyph.iter().enumerate() {
            // Coalesce each row into horizontal runs: far fewer path commands
            // than one rect per pixel, and identical output.
            let mut x = 0;
            while x < DIGIT_W {
                if bits >> (7 - x) & 1 == 0 {
                    x += 1;
                    continue;
                }
                let start = x;
                while x < DIGIT_W && bits >> (7 - x) & 1 == 1 {
                    x += 1;
                }
                write!(w, "M{start} {y}h{}v1h-{}z", x - start, x - start)?;
            }
        }
        w.write_str("\"/>")?;
    }
    w.write_str("</defs></svg>")
}

/// Draws `hh:mm` using the panel's digits.
fn clock<W: Write>(w: &mut W, hour: u8, minute: u8) -> Result {
    write!(
        w,
        "<svg class=clock viewBox=\"0 0 40 {DIGIT_H}\" \
         fill=\"#fff\" role=\"img\" aria-label=\"{hour:02}:{minute:02}\">"
    )?;
    for (i, d) in [hour / 10, hour % 10, minute / 10, minute % 10]
        .iter()
        .enumerate()
    {
        // Two digits, a gap for the colon, then two more.
        let x = [0, 9, 23, 32][i];
        // Quote every value: an unquoted one would absorb the `/` of `/>`,
        // leaving the tag unclosed so the next `<use>` nests inside it.
        write!(w, "<use href=\"#d{d}\" x=\"{x}\" y=\"0\"/>")?;
    }
    // The colon, matching the two 2x2 squares the panel draws.
    write!(
        w,
        "<rect x=\"18\" y=\"4\" width=\"2\" height=\"2\"/>\
         <rect x=\"18\" y=\"9\" width=\"2\" height=\"2\"/></svg>"
    )
}

/// Draws the alarm time controls with up/down arrows above and below the hour and minutes.
fn alarm_clock<W: Write>(w: &mut W, hour: u8, minute: u8, off: bool) -> Result {
    let dis = if off { " disabled" } else { "" };
    let off_cls = if off { " off" } else { "" };
    write!(
        w,
        "<div class=\"clock-spin{off_cls}\" id=cs>\
         <div class=clock-col>\
         <button type=button class=spin-btn data-spin=h data-dir=1{dis} aria-label=\"Hour up\">\
         <svg viewBox=\"0 0 24 8\" width=36 height=10 fill=currentColor><polygon points=\"12,0 24,8 0,8\"/></svg></button>\
         <svg class=clock-digits viewBox=\"0 0 17 {DIGIT_H}\" fill=\"#fff\" role=\"img\" aria-label=\"{hour:02}\">\
         <use id=u0 href=\"#d{}\" x=\"0\" y=\"0\"/>\
         <use id=u1 href=\"#d{}\" x=\"9\" y=\"0\"/>\
         </svg>\
         <button type=button class=spin-btn data-spin=h data-dir=-1{dis} aria-label=\"Hour down\">\
         <svg viewBox=\"0 0 24 8\" width=36 height=10 fill=currentColor><polygon points=\"0,0 24,0 12,8\"/></svg></button>\
         </div>\
         <svg class=clock-colon viewBox=\"0 0 4 {DIGIT_H}\" fill=\"#fff\">\
         <rect x=\"1\" y=\"4\" width=\"2\" height=\"2\"/>\
         <rect x=\"1\" y=\"9\" width=\"2\" height=\"2\"/>\
         </svg>\
         <div class=clock-col>\
         <button type=button class=spin-btn data-spin=m data-dir=1{dis} aria-label=\"Minute up\">\
         <svg viewBox=\"0 0 24 8\" width=36 height=10 fill=currentColor><polygon points=\"12,0 24,8 0,8\"/></svg></button>\
         <svg class=clock-digits viewBox=\"0 0 17 {DIGIT_H}\" fill=\"#fff\" role=\"img\" aria-label=\"{minute:02}\">\
         <use id=u2 href=\"#d{}\" x=\"0\" y=\"0\"/>\
         <use id=u3 href=\"#d{}\" x=\"9\" y=\"0\"/>\
         </svg>\
         <button type=button class=spin-btn data-spin=m data-dir=-1{dis} aria-label=\"Minute down\">\
         <svg viewBox=\"0 0 24 8\" width=36 height=10 fill=currentColor><polygon points=\"0,0 24,0 12,8\"/></svg></button>\
         </div></div>\
         <input type=hidden id=h name=hour value=\"{hour}\">\
         <input type=hidden id=m name=minute value=\"{minute}\">",
        hour / 10,
        hour % 10,
        minute / 10,
        minute % 10,
    )
}

/// Formats a duration as the largest sensible unit.
fn ago<W: Write>(w: &mut W, secs: u64) -> Result {
    match secs {
        0..=99 => write!(w, "{secs}s"),
        100..=5999 => write!(w, "{}m", secs / 60),
        6000..=172_799 => write!(w, "{}h", secs / 3600),
        _ => write!(w, "{}d", secs / 86400),
    }
}

/// Writes `name` with spaces around slashes and underscores replaced with spaces,
/// e.g. `America / New York`.
fn write_zone_label<W: Write>(w: &mut W, name: &str) -> Result {
    let mut first = true;
    for part in name.split('/') {
        if !first {
            w.write_str(" / ")?;
        }
        first = false;
        for c in part.chars() {
            if c == '_' {
                w.write_char(' ')?;
            } else {
                match c {
                    '&' => w.write_str("&amp;")?,
                    '<' => w.write_str("&lt;")?,
                    '>' => w.write_str("&gt;")?,
                    '"' => w.write_str("&quot;")?,
                    '\'' => w.write_str("&#39;")?,
                    ch => w.write_char(ch)?,
                }
            }
        }
    }
    Ok(())
}

/// Renders the status and settings page.
pub fn status<W: Write>(w: &mut W, s: &Status) -> Result {
    head(w, "morrow", s.firmware, true, None)?;

    // ---- status ----
    w.write_str("<section><h2>status</h2><div class=body>")?;
    match s.now {
        Some(t) => {
            write!(
                w,
                "<div class=date>{:04}-{:02}-{:02}</div>",
                t.year, t.month, t.day
            )?;
            clock(w, t.hour, t.minute)?;
        }
        None => {
            w.write_str("<p class=none>waiting for ")?;
            escape(w, s.ntp_server)?;
            w.write_str("</p>")?;
        }
    }
    w.write_str("<div class=row><div><span class=lbl>timezone</span><p class=val>")?;
    write_zone_label(w, s.tz_name)?;
    w.write_str("</p></div><div><span class=lbl>ssid</span><p class=val>")?;
    if s.ssid.is_empty() {
        w.write_str("-")?;
    } else {
        escape(w, s.ssid)?;
    }
    w.write_str("</p></div></div><div class=row><div><span class=lbl>ntp</span><p class=val>")?;
    escape(w, s.ntp_server)?;
    w.write_str(" \u{b7} ")?;
    match s.last_sync_ago {
        Some(a) => {
            ago(w, a)?;
            w.write_str(" ago")?;
        }
        None => w.write_str("never")?,
    }
    w.write_str("</p></div><div><span class=lbl>ip</span><p class=val>")?;
    if let Some(ip) = s.ip {
        write!(w, "{}.{}.{}.{}", ip[0], ip[1], ip[2], ip[3])?;
    } else {
        w.write_str("-")?;
    }
    w.write_str("</p></div></div></div></section>")?;

    // ---- alarm ----
    w.write_str("<section><h2>alarm</h2><div class=body>")?;
    let off = s.alarm.mode == 0;
    w.write_str("<form method=post action=/alarm data-dirty>\
                 <select id=mo name=mode aria-label=repeats>")?;
    // Wording matches the panel's own label bitmap, which is carried over from
    // the MicroPython firmware and cannot change without redrawing it.
    for (i, label) in ["off", "weekdays", "weekends", "every day"].iter().enumerate() {
        let sel = if i as u8 == s.alarm.mode { " selected" } else { "" };
        write!(w, "<option value={i}{sel}>{label}</option>")?;
    }
    w.write_str("</select>")?;
    alarm_clock(w, s.alarm.hour, s.alarm.minute, off)?;
    write!(
        w,
        "<div class=row><div><label for=sr>sunrise time (min)</label>\
         <input id=sr name=sunrise type=number min=1 max=240 value={}></div>\
         <div><label for=ho>hold time (min)</label>\
         <input id=ho name=hold type=number min=0 max=240 value={}></div></div>\
         <label for=lc>number of leds</label>\
         <input id=lc name=leds type=number min=1 max=1000 value={}>\
         <button type=submit>save alarm</button></form></div></section>",
        s.alarm.sunrise_secs / 60,
        s.alarm.hold_secs / 60,
        s.alarm.led_count,
    )?;

    // ---- timezone ----
    w.write_str("<section><h2>timezone</h2><div class=body>\
                 <form method=post action=/tz data-dirty>\
                 <select id=z name=zone aria-label=zone>")?;
    let mut custom_selected = true;
    for (name, rule) in s.zones {
        let sel = if *name == s.tz_name {
            custom_selected = false;
            " selected"
        } else {
            ""
        };
        w.write_str("<option value=\"")?;
        escape(w, name)?;
        w.write_str("\" data-rule=\"")?;
        escape(w, rule)?;
        write!(w, "\"{sel}>")?;
        write_zone_label(w, name)?;
        w.write_str("</option>")?;
    }
    let csel = if custom_selected { " selected" } else { "" };
    write!(w, "<option value=\"Custom\"{csel}>Custom</option></select>")?;
    let ro = if custom_selected { "" } else { " readonly" };
    let hid = if custom_selected { "" } else { " hidden" };
    write!(w, "<div id=rw{hid}><label for=r>posix rule</label><input id=r name=rule value=\"")?;
    escape(w, s.tz_rule)?;
    write!(w, "\"{ro}></div>")?;
    w.write_str("<button type=submit>save timezone</button></form></div></section>")?;

    // ---- system ----
    w.write_str("<section><h2>system</h2><div class=body>")?;
    if s.pending_confirm {
        w.write_str(
            "<p class=note>This image has not been confirmed yet. If the device \
             resets before it does, the previous firmware comes back.</p>",
        )?;
    }
    w.write_str(OTA_FORM)?;
    w.write_str(
        "<form method=post action=/provision>\
         <button class=ghost type=submit>enter setup mode</button>\
         </form></div></section>",
    )?;

    w.write_str(PENDING_JS)?;
    foot(w)
}

/// The firmware upload control, and the script that drives it.
///
/// This is the one page that uses JavaScript, and it is confined to the status
/// page — never the captive portal, where iOS's cut-down browser cannot be
/// relied on. A plain form cannot post a raw binary body without multipart,
/// which would be far more code to parse on the device than this is to ship.
const OTA_FORM: &str = "\
<div class=file-row>\
<label for=fw id=fn class=file-lbl>Select firmware image</label>\
<button type=button class=browse-btn onclick=\"document.getElementById('fw').click()\">browse</button>\
<input id=fw type=file accept=\".bin\" style=\"display:none\">\
</div>\
<button id=go type=button disabled>update firmware</button>\
<p class=note id=st></p>\
<script>(function(){\
var T=new Int32Array(256),i,j,c;\
for(i=0;i<256;i++){c=i;for(j=0;j<8;j++)c=c&1?0xEDB88320^(c>>>1):c>>>1;T[i]=c}\
function crc(b){var c=-1,i;for(i=0;i<b.length;i++)c=T[(c^b[i])&255]^(c>>>8);return(c^-1)>>>0}\
var f=document.getElementById('fw'),fn=document.getElementById('fn'),g=document.getElementById('go'),s=document.getElementById('st');\
f.onchange=function(){var has=f.files&&f.files.length>0;fn.textContent=has?f.files[0].name:'Select firmware image';fn.style.color=has?'var(--fg)':'var(--dim)';g.disabled=!has;};\
g.onclick=function(){\
var file=f.files&&f.files[0];\
if(!file){s.textContent='Choose an image first.';return}\
var r=new FileReader();\
r.onload=function(){\
var b=new Uint8Array(r.result);\
s.textContent='Uploading '+b.length+' bytes...';\
g.disabled=true;\
fetch('/ota',{method:'POST',headers:{'X-Image-Crc32':('0000000'+crc(b).toString(16)).slice(-8)},body:b})\
.then(function(x){return x.text().then(function(t){return{ok:x.ok,t:t}})}).\
then(function(r){s.textContent=r.ok?'Accepted. Rebooting; reload in a moment.':'Rejected: '+r.t;g.disabled=!r.ok}).\
catch(function(){s.textContent='Connection closed. If the upload finished, the device is rebooting.'});\
};\
r.readAsArrayBuffer(file);\
};\
})();</script>";

/// Enables each save button only while its form holds unsaved edits, and
/// previews the alarm clock from the fields above it.
///
/// The second and last script on any page, and like the uploader it is confined
/// to the status page — never the captive portal, where iOS's cut-down browser
/// cannot be relied on. Everything it does is additive: the document ships with
/// every button enabled and the server's own choice of readout visible, so with
/// script disabled the page behaves exactly as it did before.
///
/// The preview deliberately shows unsaved state. The lit save button is what
/// distinguishes "this is the alarm" from "this is what you are about to save",
/// and a reload restores the truth either way.
const PENDING_JS: &str = "\
<script>(function(){\
function all(s,r){return (r||document).querySelectorAll(s)}\
var fs=all('form[data-dirty]'),i;\
for(i=0;i<fs.length;i++)(function(f){\
var b=f.querySelector('button[type=submit]'),c=all('input,select',f),v=[],j;\
if(!b)return;\
for(j=0;j<c.length;j++)v[j]=c[j].value;\
function s(){var k,d=0;for(k=0;k<c.length;k++)if(c[k].value!==v[k])d=1;b.disabled=!d}\
f.addEventListener('input',s);f.addEventListener('change',s);s()\
})(fs[i]);\
var g=function(n){return document.getElementById(n)},\
h=g('h'),m=g('m'),mo=g('mo'),cs=g('cs'),\
u0=g('u0'),u1=g('u1'),u2=g('u2'),u3=g('u3'),sbs=all('.spin-btn');\
var p=function(){\
var o=mo?mo.value==='0':false,hh=h?+h.value||0:0,mm=m?+m.value||0:0,k;\
if(cs)cs.classList.toggle('off',o);\
for(k=0;k<sbs.length;k++)sbs[k].disabled=o;\
if(u0&&u1&&u2&&u3){\
u0.setAttribute('href','#d'+(hh/10|0));\
u1.setAttribute('href','#d'+(hh%10));\
u2.setAttribute('href','#d'+(mm/10|0));\
u3.setAttribute('href','#d'+(mm%10));\
}\
};\
if(mo)mo.addEventListener('change',p);\
for(i=0;i<sbs.length;i++)(function(b){\
b.onclick=function(e){\
e.preventDefault();\
var id=b.getAttribute('data-spin'),inp=g(id),d=+b.getAttribute('data-dir');\
if(!inp||b.disabled)return;\
if(id==='m'){\
var cur=+inp.value||0,v=d>0?(Math.floor(cur/5)*5+5):(Math.ceil(cur/5)*5-5);\
if(v>55){\
v=0;\
if(h){var hv=(+h.value||0)+1;if(hv>23)hv=0;h.value=hv;h.dispatchEvent(new Event('input',{bubbles:true}));h.dispatchEvent(new Event('change',{bubbles:true}));}\
}else if(v<0){\
v=55;\
if(h){var hv=(+h.value||0)-1;if(hv<0)hv=23;h.value=hv;h.dispatchEvent(new Event('input',{bubbles:true}));h.dispatchEvent(new Event('change',{bubbles:true}));}\
}\
inp.value=v;\
}else{\
var v=(+inp.value||0)+d;\
if(v>23)v=0;else if(v<0)v=23;\
inp.value=v;\
}\
p();\
inp.dispatchEvent(new Event('input',{bubbles:true}));\
inp.dispatchEvent(new Event('change',{bubbles:true}));\
};\
})(sbs[i]);\
var z=g('z'),r=g('r'),rw=g('rw');\
if(z&&r){\
z.addEventListener('change',function(){\
var opt=z.options[z.selectedIndex];\
var custom=z.value==='Custom';\
if(rw)rw.hidden=!custom;\
if(!custom&&opt.getAttribute('data-rule')){\
r.value=opt.getAttribute('data-rule');\
}\
r.readOnly=!custom;\
if(custom){r.focus();}\
r.dispatchEvent(new Event('input',{bubbles:true}));\
});\
}\
})();</script>";

/// Writes `s` with characters that cannot appear in a URL query value escaped.
pub fn write_query_val<W: Write>(w: &mut W, s: &str) -> Result {
    for b in s.as_bytes() {
        match *b {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                w.write_char(*b as char)?;
            }
            b' ' => w.write_str("%20")?,
            byte => {
                const HEX: &[u8; 16] = b"0123456789ABCDEF";
                w.write_char('%')?;
                w.write_char(HEX[(byte >> 4) as usize] as char)?;
                w.write_char(HEX[(byte & 0xf) as usize] as char)?;
            }
        }
    }
    Ok(())
}

/// Renders the network list page served by the captive portal.
pub fn setup<W: Write>(w: &mut W, s: &Setup) -> Result {
    head(w, "morrow setup", s.firmware, false, None)?;

    if let Some(err) = s.last_error {
        w.write_str("<p class=warn>Could not join ")?;
        escape(w, err)?;
        w.write_str(". Check the passphrase and try again.</p>")?;
    }

    w.write_str("<section><h2>wifi</h2><div class=body>")?;

    if s.networks.is_empty() {
        w.write_str("<p class=none>no networks found</p>")?;
    } else {
        for n in s.networks {
            w.write_str("<a class=net href=\"/join?ssid=")?;
            write_query_val(w, n.ssid)?;
            w.write_str("\"><span class=who>")?;
            escape(w, n.ssid)?;
            w.write_str("</span><span class=lbl>")?;
            w.write_str(if n.secured { "secured" } else { "open" })?;
            w.write_str("</span></a>")?;
        }
    }

    w.write_str(
        "<div class=row>\
         <a class=\"button ghost\" href=/join>other</a>\
         <a class=\"button ghost\" href=/scan>rescan</a>\
         </div></div></section>",
    )?;

    foot(w)
}

/// Renders the password / credential form for joining a network.
pub fn setup_join<W: Write>(
    w: &mut W,
    version: &str,
    _hostname: &str,
    ssid: Option<&str>,
) -> Result {
    head(w, "morrow setup", version, false, None)?;

    w.write_str("<section><h2>wifi</h2><div class=body>\
                 <form method=post action=/wifi>")?;

    let has_ssid = match ssid {
        Some(s) if !s.is_empty() => {
            w.write_str("<div class=lbl>ssid</div><div class=val><b>")?;
            escape(w, s)?;
            w.write_str("</b></div><input type=hidden name=ssid value=\"")?;
            escape(w, s)?;
            w.write_str("\">")?;
            true
        }
        _ => {
            w.write_str(
                "<label for=ss>ssid</label>\
                 <input id=ss name=ssid autocapitalize=none autocorrect=off required autofocus>",
            )?;
            false
        }
    };

    let pw_focus = if has_ssid { " autofocus" } else { "" };
    write!(
        w,
        "<label for=pw>passphrase</label>\
         <input id=pw name=psk type=password autocapitalize=none minlength=8 required{pw_focus}>\
         <button type=submit>connect</button>\
         </form></div></section>",
    )?;

    foot(w)
}

/// Renders the departure page shown when credentials have been submitted.
pub fn connecting<W: Write>(w: &mut W, version: &str, ssid: &str) -> Result {
    head(w, "morrow setup", version, false, None)?;
    w.write_str(
        "<meta http-equiv=refresh content=\"8;url=http://morrow.local/\">\
         <section><h2>wifi</h2><div class=\"body ctr\"><p>Connecting to <b>",
    )?;
    escape(w, ssid)?;
    w.write_str(
        "</b>\u{2026}</p>\
         <p class=note>This setup network will disconnect now.</p>\
         <p class=note>Visit <b>morrow.local</b> to complete the setup.</p>\
         </div></section>",
    )?;
    foot(w)
}

/// Renders the page shown while a rescan runs.
///
/// Carries a one-shot meta refresh back to `/`, long enough for the radio to
/// finish: the request only sets a flag, the scan itself happens in the wifi
/// task and takes about a second and a half. Nothing is typed in yet at this
/// point, so coming back to a fresh page costs nothing.
pub fn scanning<W: Write>(w: &mut W, version: &str) -> Result {
    head(w, "morrow setup", version, false, Some(4))?;
    w.write_str(
        "<section><h2>wifi</h2><div class=body>\
         <p class=none>scanning\u{2026}</p>\
         </div></section>",
    )?;
    foot(w)
}

/// Renders a short confirmation page with a link back.
pub fn message<W: Write>(w: &mut W, version: &str, title: &str, body: &str, back: &str) -> Result {
    head(w, title, version, false, None)?;
    w.write_str("<section><h2>")?;
    escape(w, title)?;
    w.write_str("</h2><div class=body><p>")?;
    escape(w, body)?;
    w.write_str("</p><p class=note><a href=\"")?;
    escape(w, back)?;
    w.write_str("\">back</a></p></div></section>")?;
    foot(w)
}
