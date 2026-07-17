mod model;
mod view;

pub(crate) use model::{TuiAttachmentModel, TuiAttachmentPasteDisposition};
pub(crate) use view::{
    init, TuiAttachmentBar, TuiAttachmentBarEvent, FOCUS_ATTACHMENTS_BINDING_NAME,
};
