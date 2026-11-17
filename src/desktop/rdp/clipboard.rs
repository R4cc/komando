//! RDP's clipboard channel (CLIPRDR, MS-RDPECLIP), for text only.
//!
//! The channel works by announcement: each side says what it has on offer, and the other asks
//! for the data when it pastes. Komando offers this PC's text whenever the view sends it (the
//! desktop has just taken the keyboard), and asks for the desktop's text as soon as the
//! desktop announces some, so it is on this PC's clipboard before anyone pastes.
//!
//! IronRDP calls the backend while it processes the server's messages, where nothing can be
//! sent; the backend only notes what happened, and `Clipboard::drain` answers afterwards. Each
//! step returns the channel's messages, which the session loop encodes and sends.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use ironrdp_cliprdr::CliprdrClient;
use ironrdp_cliprdr::backend::CliprdrBackend;
use ironrdp_cliprdr::pdu::{
    ClipboardFormat, ClipboardFormatId, ClipboardGeneralCapabilityFlags, FileContentsRequest, FileContentsResponse, FormatDataRequest,
    FormatDataResponse, LockDataId, OwnedFormatDataResponse,
};
use ironrdp_svc::SvcMessage;

/// What the channel told Komando while the server's messages were processed.
#[derive(Debug, PartialEq)]
enum Note {
    /// The channel starts (or the server asks again): say what this PC offers.
    Announce,
    Ready,
    /// The desktop copied something. The text format to ask for, if it has text.
    Copied(Option<ClipboardFormatId>),
    /// The desktop pastes what this PC offered, in this format.
    Wanted(ClipboardFormatId),
    /// The text asked for arrived, or `None`: the desktop couldn't give it.
    Arrived(Option<Vec<u8>>),
}

type Notes = Arc<Mutex<Vec<Note>>>;

// Keep our queue below IronRDP's limit, where it reports a synchronous failure instead
// of sending a request. A server that stops answering cannot grow the queue indefinitely.
const MAX_PENDING_TEXT_REQUESTS: usize = 32;

#[derive(Debug, Default)]
struct Backend {
    notes: Notes,
}

ironrdp_core::impl_as_any!(Backend);

impl Backend {
    fn note(&self, note: Note) {
        self.notes.lock().expect("clipboard notes").push(note);
    }
}

impl CliprdrBackend for Backend {
    fn temporary_directory(&self) -> &str {
        // Only used for copying files, which Komando doesn't offer.
        ".cliprdr"
    }

    fn client_capabilities(&self) -> ClipboardGeneralCapabilityFlags {
        // Text only: no files, so no streams or locks.
        ClipboardGeneralCapabilityFlags::USE_LONG_FORMAT_NAMES
    }

    fn on_ready(&mut self) {
        self.note(Note::Ready);
    }

    fn on_request_format_list(&mut self) {
        self.note(Note::Announce);
    }

    fn on_process_negotiated_capabilities(&mut self, _: ClipboardGeneralCapabilityFlags) {}

    fn on_remote_copy(&mut self, formats: &[ClipboardFormat]) {
        let has = |id: ClipboardFormatId| formats.iter().any(|f| f.id() == id);
        // Unicode first: CF_TEXT is the ANSI code page's, which loses anything outside it.
        let text = [ClipboardFormatId::CF_UNICODETEXT, ClipboardFormatId::CF_TEXT].into_iter().find(|id| has(*id));
        self.note(Note::Copied(text));
    }

    fn on_format_data_request(&mut self, request: FormatDataRequest) {
        self.note(Note::Wanted(request.format));
    }

    fn on_format_data_response(&mut self, response: FormatDataResponse<'_>) {
        self.note(Note::Arrived((!response.is_error()).then(|| response.data().to_vec())));
    }

    // Never asked for: Komando offers no files.
    fn on_file_contents_request(&mut self, _: FileContentsRequest) {}
    fn on_file_contents_response(&mut self, _: FileContentsResponse<'_>) {}
    fn on_lock(&mut self, _: LockDataId) {}
    fn on_unlock(&mut self, _: LockDataId) {}
}

/// Komando's side of the channel, kept by the session loop.
pub(super) struct Clipboard {
    notes: Notes,
    /// Whether text goes either way (the desktop's Share clipboard option).
    sharing: bool,
    /// This PC's text on offer to the desktop, with CRLF line breaks.
    local: Option<String>,
    /// Replies arrive in request order. Disabling sharing invalidates pending replies.
    asked: VecDeque<Option<ClipboardFormatId>>,
    /// The offer changed while the initial format list was awaiting acknowledgement.
    announce_when_ready: bool,
}

impl Clipboard {
    /// The channel to attach to the connection, and Komando's side of it.
    pub(super) fn new(sharing: bool) -> (CliprdrClient, Self) {
        let backend = Backend::default();
        let notes = backend.notes.clone();
        (CliprdrClient::new(Box::new(backend)), Self {
            notes, sharing, local: None, asked: VecDeque::new(), announce_when_ready: false,
        })
    }

    /// This PC's clipboard holds `text`: offer it. Before the channel is ready it waits for
    /// the server's first request (`Note::Announce`).
    pub(super) fn offer(&mut self, channel: &mut CliprdrClient, text: String) -> Vec<SvcMessage> {
        if !self.sharing {
            return vec![];
        }
        self.local = Some(super::super::with_crlf(&text));
        self.update_offer(channel)
    }

    /// Start or stop sharing. Stopping withdraws the text on offer.
    pub(super) fn share(&mut self, channel: &mut CliprdrClient, on: bool) -> Vec<SvcMessage> {
        if self.sharing == on {
            return vec![];
        }
        self.sharing = on;
        if on {
            return vec![];
        }
        self.local = None;
        self.asked.iter_mut().for_each(|format| *format = None);
        self.update_offer(channel)
    }

    /// Answer what the channel noted while the server's messages were processed. Text that
    /// arrived from the desktop is handed to `arrived`.
    pub(super) fn drain(&mut self, channel: &mut CliprdrClient, mut arrived: impl FnMut(String)) -> Vec<SvcMessage> {
        let notes = std::mem::take(&mut *self.notes.lock().expect("clipboard notes"));
        let mut messages = vec![];
        for note in notes {
            match note {
                // During setup the first announcement is what completes the channel.
                Note::Announce => {
                    self.announce_when_ready = false;
                    messages.extend(self.announce(channel));
                }
                Note::Ready => {
                    if std::mem::take(&mut self.announce_when_ready) {
                        messages.extend(self.announce(channel));
                    }
                }
                Note::Copied(Some(format)) if self.sharing && self.asked.len() < MAX_PENDING_TEXT_REQUESTS => {
                    let request = step(channel.initiate_paste(format));
                    if !request.is_empty() {
                        self.asked.push_back(Some(format));
                        messages.extend(request);
                    }
                }
                Note::Copied(_) => {}
                Note::Wanted(format) => {
                    let response = match (&self.local, format) {
                        (Some(text), ClipboardFormatId::CF_UNICODETEXT) if self.sharing => {
                            OwnedFormatDataResponse::new_unicode_string(text)
                        }
                        // Not what was offered, or no longer shared.
                        _ => OwnedFormatDataResponse::new_error(),
                    };
                    messages.extend(step(channel.submit_format_data(response)));
                }
                Note::Arrived(data) => {
                    let format = self.asked.pop_front().flatten();
                    if let (Some(data), Some(format), true) = (data, format, self.sharing)
                        && let Some(text) = decode(data, format)
                    {
                        arrived(text);
                    }
                }
            }
        }
        messages
    }

    fn update_offer(&mut self, channel: &mut CliprdrClient) -> Vec<SvcMessage> {
        if channel.is_ready() {
            self.announce(channel)
        } else {
            self.announce_when_ready = true;
            vec![]
        }
    }

    /// Tell the desktop what this PC offers: its text, or nothing.
    fn announce(&self, channel: &mut CliprdrClient) -> Vec<SvcMessage> {
        let formats: Vec<ClipboardFormat> = self
            .local
            .iter()
            .filter(|_| self.sharing)
            .map(|_| ClipboardFormat::new(ClipboardFormatId::CF_UNICODETEXT))
            .collect();
        step(channel.initiate_copy(&formats))
    }
}

/// What a step of the channel sends. A clipboard failure is logged rather than ending the
/// desktop: it is a convenience, the session is what matters.
fn step(result: ironrdp_pdu::PduResult<ironrdp_cliprdr::CliprdrSvcMessages<ironrdp_cliprdr::Client>>) -> Vec<SvcMessage> {
    match result {
        Ok(messages) => messages.into(),
        Err(err) => {
            log::warn!("remote desktop clipboard: {err}");
            vec![]
        }
    }
}

/// The desktop's text in `format`, with this PC's line breaks.
fn decode(data: Vec<u8>, format: ClipboardFormatId) -> Option<String> {
    let response = FormatDataResponse::new_data(data);
    let text = match format {
        ClipboardFormatId::CF_UNICODETEXT => response.to_unicode_string(),
        _ => response.to_string(),
    };
    text.ok().map(|text| super::super::for_this_pc(&text))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_desktops_text_is_read_in_the_format_asked_for() {
        let unicode = OwnedFormatDataResponse::new_unicode_string("grüße\r\nzwei").into_data().into_owned();
        let expected = super::super::super::for_this_pc("grüße\r\nzwei");
        assert_eq!(decode(unicode, ClipboardFormatId::CF_UNICODETEXT).as_deref(), Some(expected.as_str()));
        let ansi = OwnedFormatDataResponse::new_string("plain").into_data().into_owned();
        assert_eq!(decode(ansi, ClipboardFormatId::CF_TEXT).as_deref(), Some("plain"));
    }

    use ironrdp_cliprdr::CliprdrServer;
    use ironrdp_svc::SvcProcessor;

    /// Hand `messages` to the other side of the channel, as the connection would, and return
    /// what it answers.
    fn deliver(to: &mut impl SvcProcessor, messages: Vec<SvcMessage>) -> Vec<SvcMessage> {
        messages.iter().flat_map(|m| to.process(&m.encode_unframed_pdu().unwrap()).unwrap()).collect()
    }

    fn take(notes: &Notes) -> Vec<Note> {
        std::mem::take(&mut *notes.lock().unwrap())
    }

    /// Komando's side against IronRDP's own server side of the channel, with real messages.
    #[test]
    fn text_goes_both_ways_through_the_channel_until_sharing_stops() {
        let unicode = ClipboardFormatId::CF_UNICODETEXT;
        let (mut client, mut ours) = Clipboard::new(true);
        let server_notes = Notes::default();
        let mut server = CliprdrServer::new(Box::new(Backend { notes: server_notes.clone() }));
        let none = |_: String| panic!("nothing was copied on the desktop");

        // A copy after the initial announcement still needs to be offered once it is accepted.
        let start = server.start().unwrap();
        assert!(deliver(&mut client, start).is_empty());
        let announced = ours.drain(&mut client, none);
        let acknowledged = deliver(&mut server, announced);
        assert_eq!(take(&server_notes), vec![Note::Ready, Note::Copied(None)]);
        assert!(ours.offer(&mut client, "local\ntext".into()).is_empty());
        assert!(deliver(&mut client, acknowledged).is_empty());
        assert!(client.is_ready());
        let announced = ours.drain(&mut client, none);
        let acknowledged = deliver(&mut server, announced);
        assert_eq!(take(&server_notes), vec![Note::Copied(Some(unicode))]);
        deliver(&mut client, acknowledged);

        // The desktop pastes: it gets this PC's text, with Windows line breaks.
        let request = server.initiate_paste(unicode).unwrap().into();
        assert!(deliver(&mut client, request).is_empty());
        let data = ours.drain(&mut client, none);
        assert!(deliver(&mut server, data).is_empty());
        let [Note::Arrived(Some(bytes))] = &take(&server_notes)[..] else { panic!("the desktop got no text") };
        assert_eq!(FormatDataResponse::new_data(bytes.clone()).to_unicode_string().unwrap(), "local\r\ntext");

        // The desktop copies: Komando asks for the text at once, and it lands here.
        let copied = server.initiate_copy(&[ClipboardFormat::new(unicode)]).unwrap().into();
        deliver(&mut client, copied);
        let paste = ours.drain(&mut client, none);
        assert!(deliver(&mut server, paste).is_empty());
        assert_eq!(take(&server_notes), vec![Note::Wanted(unicode)]);
        // Another copy can be announced before the desktop replies to the first request.
        let ansi = ClipboardFormatId::CF_TEXT;
        let copied = server.initiate_copy(&[ClipboardFormat::new(ansi)]).unwrap().into();
        deliver(&mut client, copied);
        let paste = ours.drain(&mut client, none);
        assert!(deliver(&mut server, paste).is_empty());
        assert_eq!(take(&server_notes), vec![Note::Wanted(ansi)]);
        let reply = server.submit_format_data(OwnedFormatDataResponse::new_unicode_string("remote\r\ntext")).unwrap().into();
        deliver(&mut client, reply);
        let reply = server.submit_format_data(OwnedFormatDataResponse::new_string("newer text")).unwrap().into();
        deliver(&mut client, reply);
        let mut arrived = vec![];
        assert!(ours.drain(&mut client, |text| arrived.push(text)).is_empty());
        assert_eq!(arrived, vec![super::super::super::for_this_pc("remote\r\ntext"), "newer text".into()]);

        // Sharing stops: the offer is withdrawn, and a paste there gets nothing.
        let withdrawn = ours.share(&mut client, false);
        deliver(&mut server, withdrawn);
        assert_eq!(take(&server_notes), vec![Note::Copied(None)]);
        let request = server.initiate_paste(unicode).unwrap().into();
        deliver(&mut client, request);
        let refused = ours.drain(&mut client, none);
        deliver(&mut server, refused);
        assert_eq!(take(&server_notes), vec![Note::Arrived(None)]);
        // Nor does a copy there come here.
        let copied = server.initiate_copy(&[ClipboardFormat::new(unicode)]).unwrap().into();
        deliver(&mut client, copied);
        assert!(ours.drain(&mut client, none).is_empty());
    }

    #[test]
    fn a_remote_copy_asks_for_unicode_text_first() {
        let mut backend = Backend::default();
        backend.on_remote_copy(&[ClipboardFormat::new(ClipboardFormatId::CF_TEXT), ClipboardFormat::new(ClipboardFormatId::CF_UNICODETEXT)]);
        backend.on_remote_copy(&[ClipboardFormat::new(ClipboardFormatId::new(0xC0FE))]);
        let notes = std::mem::take(&mut *backend.notes.lock().unwrap());
        assert_eq!(notes, vec![Note::Copied(Some(ClipboardFormatId::CF_UNICODETEXT)), Note::Copied(None)]);
    }
}
