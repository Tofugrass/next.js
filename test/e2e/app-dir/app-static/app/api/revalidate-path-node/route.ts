import { NextRequest, NextResponse } from 'next/server'
import { expirePath } from 'next/cache'

export const revalidate = 1

export async function GET(req: NextRequest) {
  const path = req.nextUrl.searchParams.get('path') || '/'
  try {
    console.log('revalidating path', path)
    expirePath(path)
    return NextResponse.json({ revalidated: true, now: Date.now() })
  } catch (err) {
    console.error('Failed to revalidate', path, err)
    return NextResponse.json({ revalidated: false, now: Date.now() })
  }
}
