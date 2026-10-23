# Device names

The original desktop device drawer shows `This device: <name>` and an **Edit name** button. It opens the Pairing tab's **This device name** field. Enter or **Save name** persists a valid name through the existing owner-side compare-and-save operation. **Use host name** explicitly saves the current system hostname. **Cancel** discards a draft and reloads the owner's current name.

Leading and trailing spaces are trimmed at save. Names must contain 1–128 UTF-8 bytes after trimming, without control characters or line breaks. Invalid input stays editable and is not persisted. Existing custom names remain authoritative on restart, reconnect and upgrade. Resetting to the host name changes only the label; no device ID, invitation, group or permission is regenerated.

The normal owner reload updates discovery and relay registration from the saved profile. Existing active-session and file-transfer guards still apply. A save receipt is not proof that an offline peer has received the announcement.

Validation from primary main uses the guarded fixed modes `device-name`, `device-name-pairing`, `device-name-runtime`, `discovery-observation`, and `platform-macos-check` in `scripts/test_stream_settings_model.py`, plus `scripts/test_release.sh`. UI compilation, actual window interaction, signed packaging and installed-device acceptance must be reported separately.
