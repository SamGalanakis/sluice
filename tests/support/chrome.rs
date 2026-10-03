use sluice_model::error::PublicError;

pub struct Chrome;
impl Chrome {
    pub fn open(_url: &str) -> Result<Self, PublicError> {
        Err(PublicError::not_implemented("Chromium fixture"))
    }
}
