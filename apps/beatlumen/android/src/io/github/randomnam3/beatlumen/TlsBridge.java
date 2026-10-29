// TLS connections for https:// servers using Android's own TLS (no TLS
// library is bundled). Used by the native core through tls.cpp; each
// connection is opened, used and closed by one native worker thread.
//
// A certificate must be trusted by the system and match the host name, or
// have exactly the SHA-256 fingerprint the user chose to trust (self-signed
// certificates of home servers). Untrusted certificates are reported with
// their fingerprint ("[sha256:...]") so the user can compare it.
//
// SPDX-License-Identifier: AGPL-3.0-or-later

package io.github.randomnam3.beatlumen;

import java.io.IOException;
import java.io.InputStream;
import java.io.OutputStream;
import java.net.InetSocketAddress;
import java.net.Socket;
import java.net.SocketTimeoutException;
import java.security.KeyStore;
import java.security.MessageDigest;
import java.security.cert.Certificate;
import java.security.cert.CertificateException;
import java.security.cert.X509Certificate;
import java.util.Locale;
import java.util.Map;
import java.util.concurrent.ConcurrentHashMap;
import java.util.concurrent.atomic.AtomicLong;

import javax.net.ssl.HttpsURLConnection;
import javax.net.ssl.SSLContext;
import javax.net.ssl.SSLSocket;
import javax.net.ssl.TrustManager;
import javax.net.ssl.TrustManagerFactory;
import javax.net.ssl.X509TrustManager;

public final class TlsBridge {
    private TlsBridge() {}

    private static final Map<Long, SSLSocket> sConns = new ConcurrentHashMap<>();
    private static final AtomicLong sNext = new AtomicLong(1);
    private static final ThreadLocal<String> sError = new ThreadLocal<>();

    static String sha256Hex(byte[] der)
    {
        try {
            byte[] d = MessageDigest.getInstance("SHA-256").digest(der);
            StringBuilder sb = new StringBuilder();
            for (byte b : d)
                sb.append(String.format(Locale.ROOT, "%02x", b));
            return sb.toString();
        } catch (Exception e) {
            return "";
        }
    }

    /** Error message of the last failed open() on this thread. */
    public static String lastError()
    {
        String e = sError.get();
        return e == null ? "" : e;
    }

    /** Returns a handle > 0, or 0 (see lastError()). */
    public static long open(String host, int port, int timeoutMs, String pin)
    {
        final String wanted = pin == null ? "" : pin.toLowerCase(Locale.ROOT);
        final String[] seen = new String[1];
        SSLSocket sock = null;
        try {
            TrustManagerFactory tmf = TrustManagerFactory.getInstance(TrustManagerFactory.getDefaultAlgorithm());
            tmf.init((KeyStore) null);
            final X509TrustManager system = (X509TrustManager) tmf.getTrustManagers()[0];
            X509TrustManager tm = new X509TrustManager() {
                @Override public void checkClientTrusted(X509Certificate[] chain, String auth) throws CertificateException
                {
                    throw new CertificateException("not a server");
                }
                @Override public void checkServerTrusted(X509Certificate[] chain, String auth) throws CertificateException
                {
                    if (chain.length > 0)
                        seen[0] = sha256Hex(chain[0].getEncoded());
                    try {
                        system.checkServerTrusted(chain, auth);
                    } catch (CertificateException e) {
                        if (wanted.isEmpty() || !wanted.equals(seen[0]))
                            throw e;
                    }
                }
                @Override public X509Certificate[] getAcceptedIssuers()
                {
                    return system.getAcceptedIssuers();
                }
            };
            SSLContext ctx = SSLContext.getInstance("TLS");
            ctx.init(null, new TrustManager[] { tm }, null);
            Socket plain = new Socket();
            plain.connect(new InetSocketAddress(host, port), timeoutMs);
            plain.setSoTimeout(timeoutMs);
            sock = (SSLSocket) ctx.getSocketFactory().createSocket(plain, host, port, true);
            sock.startHandshake();
            // Host name: checked unless the certificate is the trusted one.
            Certificate[] peer = sock.getSession().getPeerCertificates();
            boolean pinned = !wanted.isEmpty() && peer.length > 0 && wanted.equals(sha256Hex(peer[0].getEncoded()));
            if (!pinned && !HttpsURLConnection.getDefaultHostnameVerifier().verify(host, sock.getSession()))
                throw new IOException("certificate does not match host name " + host);
            long h = sNext.getAndIncrement();
            sConns.put(h, sock);
            return h;
        } catch (Exception e) {
            String msg = e.getMessage() != null ? e.getMessage() : e.getClass().getSimpleName();
            if (e instanceof javax.net.ssl.SSLHandshakeException && seen[0] != null)
                msg = "certificate not trusted [sha256:" + seen[0] + "]";
            sError.set(msg);
            if (sock != null) {
                try {
                    sock.close();
                } catch (IOException ignored) {
                }
            }
            return 0;
        }
    }

    /** Bytes read, 0 at the end of the stream, -1 on errors or timeouts. */
    public static int read(long h, byte[] buf, int len, int timeoutMs)
    {
        SSLSocket s = sConns.get(h);
        if (s == null)
            return -1;
        try {
            s.setSoTimeout(timeoutMs);
            InputStream in = s.getInputStream();
            int n = in.read(buf, 0, len);
            return n < 0 ? 0 : n;
        } catch (SocketTimeoutException e) {
            return -2; // nothing within the timeout; the socket stays usable
        } catch (IOException e) {
            return -1;
        }
    }

    public static int write(long h, byte[] buf, int len)
    {
        SSLSocket s = sConns.get(h);
        if (s == null)
            return -1;
        try {
            OutputStream out = s.getOutputStream();
            out.write(buf, 0, len);
            out.flush();
            return len;
        } catch (IOException e) {
            return -1;
        }
    }

    public static void close(long h)
    {
        SSLSocket s = sConns.remove(h);
        if (s != null) {
            try {
                s.close();
            } catch (IOException ignored) {
            }
        }
    }
}
