use crate::message::SignalingMessage;
use thiserror::Error;

/// Connection states for the sender.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SenderState {
    Idle,
    Discovering,
    Connecting,
    Pairing,
    Authenticating,
    Signaling,
    Streaming,
    Disconnecting,
}

/// Connection states for the receiver.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReceiverState {
    Idle,
    Advertising,
    PendingConnection,
    Pairing,
    Authenticating,
    Signaling,
    Receiving,
    Disconnecting,
}

#[derive(Error, Debug)]
pub enum StateError {
    #[error("Invalid state transition: {from} → {to} (triggered by {trigger})")]
    InvalidTransition {
        from: String,
        to: String,
        trigger: String,
    },
}

/// State machine for the sender side of the connection.
#[derive(Debug)]
pub struct SenderStateMachine {
    state: SenderState,
}

impl SenderStateMachine {
    pub fn new() -> Self {
        Self {
            state: SenderState::Idle,
        }
    }

    pub fn state(&self) -> SenderState {
        self.state
    }

    /// Attempt to transition to the next state based on an event.
    pub fn transition(&mut self, event: &SenderEvent) -> Result<SenderState, StateError> {
        let next = match (self.state, event) {
            (SenderState::Idle, SenderEvent::StartDiscovery) => SenderState::Discovering,
            (SenderState::Discovering, SenderEvent::ReceiverSelected) => SenderState::Connecting,
            (SenderState::Discovering, SenderEvent::Cancel) => SenderState::Idle,
            (SenderState::Connecting, SenderEvent::Connected) => SenderState::Authenticating,
            (SenderState::Connecting, SenderEvent::NeedsPairing) => SenderState::Pairing,
            (SenderState::Connecting, SenderEvent::Timeout) => SenderState::Idle,
            (SenderState::Connecting, SenderEvent::Cancel) => SenderState::Idle,
            (SenderState::Pairing, SenderEvent::PairingComplete) => SenderState::Authenticating,
            (SenderState::Pairing, SenderEvent::PairingFailed) => SenderState::Idle,
            (SenderState::Pairing, SenderEvent::Cancel) => SenderState::Idle,
            (SenderState::Authenticating, SenderEvent::Authenticated) => SenderState::Signaling,
            (SenderState::Authenticating, SenderEvent::AuthFailed) => SenderState::Idle,
            (SenderState::Signaling, SenderEvent::StreamStarted) => SenderState::Streaming,
            (SenderState::Signaling, SenderEvent::SignalingFailed) => SenderState::Idle,
            (SenderState::Streaming, SenderEvent::Disconnect) => SenderState::Disconnecting,
            (SenderState::Streaming, SenderEvent::ConnectionLost) => SenderState::Disconnecting,
            (SenderState::Disconnecting, SenderEvent::Disconnected) => SenderState::Idle,
            _ => {
                return Err(StateError::InvalidTransition {
                    from: format!("{:?}", self.state),
                    to: "?".to_string(),
                    trigger: format!("{:?}", event),
                });
            }
        };
        tracing::debug!(from = ?self.state, to = ?next, event = ?event, "sender state transition");
        self.state = next;
        Ok(next)
    }
}

impl Default for SenderStateMachine {
    fn default() -> Self {
        Self::new()
    }
}

/// Events that drive sender state transitions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SenderEvent {
    StartDiscovery,
    ReceiverSelected,
    Cancel,
    Connected,
    NeedsPairing,
    Timeout,
    PairingComplete,
    PairingFailed,
    Authenticated,
    AuthFailed,
    StreamStarted,
    SignalingFailed,
    Disconnect,
    ConnectionLost,
    Disconnected,
}

/// State machine for the receiver side of the connection.
#[derive(Debug)]
pub struct ReceiverStateMachine {
    state: ReceiverState,
}

impl ReceiverStateMachine {
    pub fn new() -> Self {
        Self {
            state: ReceiverState::Idle,
        }
    }

    pub fn state(&self) -> ReceiverState {
        self.state
    }

    pub fn transition(&mut self, event: &ReceiverEvent) -> Result<ReceiverState, StateError> {
        let next = match (self.state, event) {
            (ReceiverState::Idle, ReceiverEvent::StartAdvertising) => ReceiverState::Advertising,
            (ReceiverState::Advertising, ReceiverEvent::IncomingConnection) => {
                ReceiverState::PendingConnection
            }
            (ReceiverState::Advertising, ReceiverEvent::StopAdvertising) => ReceiverState::Idle,
            (ReceiverState::PendingConnection, ReceiverEvent::NeedsPairing) => {
                ReceiverState::Pairing
            }
            (ReceiverState::PendingConnection, ReceiverEvent::AlreadyPaired) => {
                ReceiverState::Authenticating
            }
            (ReceiverState::PendingConnection, ReceiverEvent::Rejected) => {
                ReceiverState::Advertising
            }
            (ReceiverState::Pairing, ReceiverEvent::PairingComplete) => {
                ReceiverState::Authenticating
            }
            (ReceiverState::Pairing, ReceiverEvent::PairingFailed) => ReceiverState::Advertising,
            (ReceiverState::Authenticating, ReceiverEvent::Authenticated) => {
                ReceiverState::Signaling
            }
            (ReceiverState::Authenticating, ReceiverEvent::AuthFailed) => {
                ReceiverState::Advertising
            }
            (ReceiverState::Signaling, ReceiverEvent::StreamStarted) => ReceiverState::Receiving,
            (ReceiverState::Signaling, ReceiverEvent::SignalingFailed) => {
                ReceiverState::Advertising
            }
            (ReceiverState::Receiving, ReceiverEvent::Disconnect) => ReceiverState::Disconnecting,
            (ReceiverState::Receiving, ReceiverEvent::ConnectionLost) => {
                ReceiverState::Disconnecting
            }
            (ReceiverState::Disconnecting, ReceiverEvent::Disconnected) => {
                ReceiverState::Advertising
            }
            _ => {
                return Err(StateError::InvalidTransition {
                    from: format!("{:?}", self.state),
                    to: "?".to_string(),
                    trigger: format!("{:?}", event),
                });
            }
        };
        tracing::debug!(from = ?self.state, to = ?next, event = ?event, "receiver state transition");
        self.state = next;
        Ok(next)
    }
}

impl Default for ReceiverStateMachine {
    fn default() -> Self {
        Self::new()
    }
}

/// Events that drive receiver state transitions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReceiverEvent {
    StartAdvertising,
    StopAdvertising,
    IncomingConnection,
    NeedsPairing,
    AlreadyPaired,
    Rejected,
    PairingComplete,
    PairingFailed,
    Authenticated,
    AuthFailed,
    StreamStarted,
    SignalingFailed,
    Disconnect,
    ConnectionLost,
    Disconnected,
}

/// Maps a received signaling message to the appropriate sender event.
pub fn sender_event_from_message(msg: &SignalingMessage) -> Option<SenderEvent> {
    match msg {
        SignalingMessage::SessionAccept { .. } => Some(SenderEvent::Authenticated),
        SignalingMessage::SessionReject { .. } => Some(SenderEvent::AuthFailed),
        SignalingMessage::PairingChallenge { .. } => Some(SenderEvent::NeedsPairing),
        SignalingMessage::PairingConfirm { .. } => Some(SenderEvent::PairingComplete),
        SignalingMessage::AuthChallenge { .. } => None, // handled inline
        SignalingMessage::AuthConfirm { .. } => Some(SenderEvent::Authenticated),
        SignalingMessage::SessionEnd { .. } => Some(SenderEvent::ConnectionLost),
        _ => None,
    }
}

/// Maps a received signaling message to the appropriate receiver event.
pub fn receiver_event_from_message(msg: &SignalingMessage) -> Option<ReceiverEvent> {
    match msg {
        SignalingMessage::SessionRequest { .. } => Some(ReceiverEvent::IncomingConnection),
        SignalingMessage::PairingResponse { .. } => None, // handled inline
        SignalingMessage::AuthResponse { .. } => None,    // handled inline
        SignalingMessage::SessionEnd { .. } => Some(ReceiverEvent::ConnectionLost),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sender_happy_path() {
        let mut sm = SenderStateMachine::new();
        assert_eq!(sm.state(), SenderState::Idle);

        sm.transition(&SenderEvent::StartDiscovery).unwrap();
        assert_eq!(sm.state(), SenderState::Discovering);

        sm.transition(&SenderEvent::ReceiverSelected).unwrap();
        assert_eq!(sm.state(), SenderState::Connecting);

        sm.transition(&SenderEvent::Connected).unwrap();
        assert_eq!(sm.state(), SenderState::Authenticating);

        sm.transition(&SenderEvent::Authenticated).unwrap();
        assert_eq!(sm.state(), SenderState::Signaling);

        sm.transition(&SenderEvent::StreamStarted).unwrap();
        assert_eq!(sm.state(), SenderState::Streaming);

        sm.transition(&SenderEvent::Disconnect).unwrap();
        assert_eq!(sm.state(), SenderState::Disconnecting);

        sm.transition(&SenderEvent::Disconnected).unwrap();
        assert_eq!(sm.state(), SenderState::Idle);
    }

    #[test]
    fn test_sender_pairing_path() {
        let mut sm = SenderStateMachine::new();
        sm.transition(&SenderEvent::StartDiscovery).unwrap();
        sm.transition(&SenderEvent::ReceiverSelected).unwrap();
        sm.transition(&SenderEvent::NeedsPairing).unwrap();
        assert_eq!(sm.state(), SenderState::Pairing);

        sm.transition(&SenderEvent::PairingComplete).unwrap();
        assert_eq!(sm.state(), SenderState::Authenticating);
    }

    #[test]
    fn test_sender_invalid_transition() {
        let mut sm = SenderStateMachine::new();
        let result = sm.transition(&SenderEvent::StreamStarted);
        assert!(result.is_err());
    }

    #[test]
    fn test_receiver_happy_path() {
        let mut sm = ReceiverStateMachine::new();
        assert_eq!(sm.state(), ReceiverState::Idle);

        sm.transition(&ReceiverEvent::StartAdvertising).unwrap();
        assert_eq!(sm.state(), ReceiverState::Advertising);

        sm.transition(&ReceiverEvent::IncomingConnection).unwrap();
        assert_eq!(sm.state(), ReceiverState::PendingConnection);

        sm.transition(&ReceiverEvent::AlreadyPaired).unwrap();
        assert_eq!(sm.state(), ReceiverState::Authenticating);

        sm.transition(&ReceiverEvent::Authenticated).unwrap();
        assert_eq!(sm.state(), ReceiverState::Signaling);

        sm.transition(&ReceiverEvent::StreamStarted).unwrap();
        assert_eq!(sm.state(), ReceiverState::Receiving);

        sm.transition(&ReceiverEvent::Disconnect).unwrap();
        assert_eq!(sm.state(), ReceiverState::Disconnecting);

        sm.transition(&ReceiverEvent::Disconnected).unwrap();
        assert_eq!(sm.state(), ReceiverState::Advertising);
    }

    #[test]
    fn test_receiver_pairing_path() {
        let mut sm = ReceiverStateMachine::new();
        sm.transition(&ReceiverEvent::StartAdvertising).unwrap();
        sm.transition(&ReceiverEvent::IncomingConnection).unwrap();
        sm.transition(&ReceiverEvent::NeedsPairing).unwrap();
        assert_eq!(sm.state(), ReceiverState::Pairing);

        sm.transition(&ReceiverEvent::PairingComplete).unwrap();
        assert_eq!(sm.state(), ReceiverState::Authenticating);
    }

    #[test]
    fn test_receiver_connection_lost() {
        let mut sm = ReceiverStateMachine::new();
        sm.transition(&ReceiverEvent::StartAdvertising).unwrap();
        sm.transition(&ReceiverEvent::IncomingConnection).unwrap();
        sm.transition(&ReceiverEvent::AlreadyPaired).unwrap();
        sm.transition(&ReceiverEvent::Authenticated).unwrap();
        sm.transition(&ReceiverEvent::StreamStarted).unwrap();

        sm.transition(&ReceiverEvent::ConnectionLost).unwrap();
        assert_eq!(sm.state(), ReceiverState::Disconnecting);

        sm.transition(&ReceiverEvent::Disconnected).unwrap();
        assert_eq!(sm.state(), ReceiverState::Advertising);
    }

    #[test]
    fn test_receiver_invalid_transition() {
        let mut sm = ReceiverStateMachine::new();
        let result = sm.transition(&ReceiverEvent::StreamStarted);
        assert!(result.is_err());
    }
}
