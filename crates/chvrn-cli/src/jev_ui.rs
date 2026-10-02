use crate::{Options, Result as CliResult};
use chvrn_core::merge_advice::MergeAdviceSuggestion;
use chvrn_integrations::jev::{JevClient, JevConfig};
use chvrn_tui::{MergeAdviceError, MergeAdviceRequest, ReviewSession};
use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};
use std::sync::{Arc, mpsc};
use std::time::Duration;

type Reply = Result<MergeAdviceSuggestion, String>;

struct ActiveRequest {
    request: MergeAdviceRequest,
    response: mpsc::Receiver<Reply>,
}

pub struct JevUi {
    client: Arc<JevClient>,
    active: Option<ActiveRequest>,
}

impl JevUi {
    pub fn from_options(options: &Options) -> CliResult<Option<Self>> {
        if !options.jev {
            return Ok(None);
        }
        if !options.interactive() {
            return Err("--jev requires an interactive session; output unchanged".into());
        }
        let api_key = std::env::var("TYPESAFE_API_KEY")
            .map_err(|_| "--jev requires TYPESAFE_API_KEY to be set to a valid UTF-8 API key")?;
        if api_key.trim().is_empty() {
            return Err("--jev requires a non-empty TYPESAFE_API_KEY".into());
        }
        Ok(Some(Self::new(JevClient::new(JevConfig {
            api_key,
            endpoint: "https://api.typesafe.ai/v1/systemone".into(),
            timeout: Duration::from_secs(30),
        })?)))
    }

    pub fn new(client: JevClient) -> Self {
        Self {
            client: Arc::new(client),
            active: None,
        }
    }

    pub fn input(&mut self, session: &mut ReviewSession, event: &Event) -> bool {
        let Event::Key(key) = event else {
            return false;
        };
        if key.kind != KeyEventKind::Press
            || key.code != KeyCode::Char('J')
            || key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER)
            || session.is_editing()
            || session.is_review_modal()
        {
            return false;
        }
        if self.active.is_some() {
            session.set_message("A suggestion request is already running");
            return true;
        }
        let request = match session.begin_merge_advice() {
            Ok(request) => request,
            Err(error) => {
                session.set_message(match error {
                    MergeAdviceError::Disabled => {
                        "Suggestions are disabled; use --jev to enable them"
                    }
                    MergeAdviceError::Unavailable => {
                        "Select an unedited unresolved conflict before requesting a suggestion"
                    }
                    MergeAdviceError::Busy => "A suggestion request is already running",
                });
                return true;
            }
        };
        let input = request.input().clone();
        let client = Arc::clone(&self.client);
        let (send, response) = mpsc::sync_channel(1);
        match std::thread::Builder::new().spawn(move || {
            let reply = client.suggest(&input).map_err(|error| error.to_string());
            let _ = send.send(reply);
        }) {
            Ok(_) => {
                self.active = Some(ActiveRequest { request, response });
                session.set_message("Requesting suggestion; editing and quit remain available");
            }
            Err(_) => {
                session.receive_merge_advice(
                    request,
                    Err("Could not start suggestion request".into()),
                );
            }
        }
        true
    }

    pub fn tick(&mut self, session: &mut ReviewSession) -> bool {
        let Some(active) = self.active.as_ref() else {
            return false;
        };
        let reply = match active.response.try_recv() {
            Ok(reply) => reply,
            Err(mpsc::TryRecvError::Empty) => return false,
            Err(mpsc::TryRecvError::Disconnected) => {
                Err("Suggestion request stopped before returning advice".into())
            }
        };
        if let Some(active) = self.active.take() {
            session.receive_merge_advice(active.request, reply);
        }
        true
    }
}
