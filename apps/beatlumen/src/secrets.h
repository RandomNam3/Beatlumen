// The platform's protected store for the vault's "stay unlocked on this
// device" (BlCallbacks secret_store/secret_load): Android Keystore
// (SecretStore.java), Windows DPAPI, the Secret Service on Linux (GNOME
// Keyring, KWallet). Read results come back as the "vault.secret" command.
#pragma once

#include "core.h"

class Bridge;

namespace secrets {
// Fills the secret_* callbacks where the platform has a store.
void configure(BlCallbacks &cb, Bridge *bridge);
} // namespace secrets
